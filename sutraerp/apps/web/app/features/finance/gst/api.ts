/**
 * ERP_EXTENSIONS 5D API layer (GST tax invoices, bills of supply, credit notes): one interface,
 * an HTTP implementation (CSRF on writes, problem+json → `ApiProblem`) and an in-memory mock for
 * mock mode. The paths are plain strings checked against docs/extensions-5d-gst.md.
 */
import { ApiProblem, isProblem, isUnsafeMethod } from "@sutra/api-client";
import { getCsrfToken, USES_API } from "../../../lib/api";
import type {
  GstFeeHead,
  GstFeeHeadTaxInput,
  GstParty,
  GstPartyInput,
  GstPeriod,
  GstProfile,
  GstProfileInput,
  GstSummary,
  GstTaxDocument,
} from "./types";

const B = "/api/v1/finance";
export const GST_PATHS = {
  profile: `${B}/institutions/{id}/gst-profile`,
  feeHeads: `${B}/institutions/{id}/gst-fee-heads`,
  feeHead: `${B}/institutions/{id}/gst-fee-heads/{feeHeadId}`,
  party: `${B}/student-accounts/{id}/gst-details`,
  chargeDocument: `${B}/charges/{id}/tax-document`,
  summary: `${B}/institutions/{id}/gst-summary`,
  summaryCsv: `${B}/institutions/{id}/gst-summary/csv`,
} as const;

export interface GstCsv {
  fileName: string;
  text: string;
}

export interface GstApi {
  profile(institutionId: string): Promise<GstProfile>;
  saveProfile(institutionId: string, input: GstProfileInput): Promise<GstProfile>;
  feeHeads(institutionId: string): Promise<GstFeeHead[]>;
  saveFeeHead(institutionId: string, feeHeadId: string, input: GstFeeHeadTaxInput): Promise<GstFeeHead>;
  clearFeeHead(institutionId: string, feeHeadId: string): Promise<void>;
  party(studentAccountId: string): Promise<GstParty>;
  saveParty(studentAccountId: string, input: GstPartyInput): Promise<GstParty>;
  /** The charge's tax invoice / bill of supply; `null` when it has none (404). */
  chargeDocument(chargeId: string): Promise<GstTaxDocument | null>;
  summary(institutionId: string, period: GstPeriod): Promise<GstSummary>;
  summaryCsv(institutionId: string, period: GstPeriod): Promise<GstCsv>;
}

const fill = (t: string, id: string, feeHeadId = "") => t.replace("{id}", encodeURIComponent(id)).replace("{feeHeadId}", encodeURIComponent(feeHeadId));
const periodQuery = (p: GstPeriod) => new URLSearchParams({ from: p.from, to: p.to }).toString();

/** `gst-summary-2026-10-01-to-2026-10-19.csv`, the server's own file name. */
export const csvFileName = (p: GstPeriod) => `gst-summary-${p.from}-to-${p.to}.csv`;

/** The file name of a `Content-Disposition: attachment; filename="…"` header, else `fallback`. */
export function dispositionName(header: string | null, fallback: string): string {
  const m = header?.match(/filename="?([^";]+)"?/i);
  return m?.[1] ?? fallback;
}

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function createHttpGstApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): GstApi {
  async function raw(method: string, path: string, body?: unknown, accept = "application/json"): Promise<Response> {
    const headers: Record<string, string> = { Accept: accept };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    return fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe && body !== undefined ? JSON.stringify(body) : undefined });
  }
  async function fail(res: Response): Promise<never> {
    const text = await res.text().catch(() => "");
    let data: unknown;
    try {
      data = text ? JSON.parse(text) : undefined;
    } catch {
      data = undefined;
    }
    if (isProblem(data)) throw new ApiProblem(data);
    throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
  }
  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const res = await raw(method, path, body ?? (isUnsafeMethod(method) && method !== "DELETE" ? {} : undefined));
    if (!res.ok) return fail(res);
    const text = await res.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }
  return {
    profile: (inst) => request("GET", fill(GST_PATHS.profile, inst)),
    saveProfile: (inst, input) => request("PUT", fill(GST_PATHS.profile, inst), input),
    feeHeads: (inst) => request("GET", fill(GST_PATHS.feeHeads, inst)),
    saveFeeHead: (inst, head, input) => request("PUT", fill(GST_PATHS.feeHead, inst, head), input),
    clearFeeHead: (inst, head) => request("DELETE", fill(GST_PATHS.feeHead, inst, head)),
    party: (id) => request("GET", fill(GST_PATHS.party, id)),
    saveParty: (id, input) => request("PUT", fill(GST_PATHS.party, id), input),
    chargeDocument: async (id) => {
      const res = await raw("GET", fill(GST_PATHS.chargeDocument, id));
      if (res.status === 404) return null;
      if (!res.ok) return fail(res);
      return (await res.json()) as GstTaxDocument;
    },
    summary: (inst, p) => request("GET", `${fill(GST_PATHS.summary, inst)}?${periodQuery(p)}`),
    summaryCsv: async (inst, p) => {
      const res = await raw("GET", `${fill(GST_PATHS.summaryCsv, inst)}?${periodQuery(p)}`, undefined, "text/csv");
      if (!res.ok) return fail(res);
      return { fileName: dispositionName(res.headers.get("content-disposition"), csvFileName(p)), text: await res.text() };
    },
  };
}

/** Hands a CSV to the browser as a download (nothing to do outside a browser). */
export function saveCsv(csv: GstCsv): void {
  if (typeof URL.createObjectURL !== "function") return;
  const url = URL.createObjectURL(new Blob([csv.text], { type: "text/csv;charset=utf-8" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = csv.fileName;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

// ------------------------------------------------------------------ mock mode

const PRESETS = [
  { id: "preset:educationExempt", label: "Education services to students (exempt)", text: "Exempt supply: services provided by an educational institution to its students, faculty and staff (Notification No. 12/2017-Central Tax (Rate), Sl. No. 66; SAC 9992)." },
  { id: "preset:exempt", label: "Exempt supply", text: "Exempt supply under the GST law; no tax is charged on this supply." },
  { id: "preset:nilRated", label: "Nil-rated supply", text: "Nil-rated supply: GST is charged at 0% on this supply." },
];
const STATES = [
  { code: "27", name: "Maharashtra" },
  { code: "29", name: "Karnataka" },
  { code: "24", name: "Gujarat" },
];
const zero = { taxableValue: "0.00", cgst: "0.00", sgst: "0.00", igst: "0.00", totalTax: "0.00" };

export function createMockGstApi(): GstApi {
  let profile: GstProfile = {
    institutionId: "",
    configured: false,
    registrationType: "regular",
    gstin: null,
    legalName: "",
    tradeName: "",
    address: "",
    stateCode: "27",
    stateName: "Maharashtra",
    defaultTaxNote: "preset:educationExempt",
    invoicePrefix: "TI/{FY}/",
    billPrefix: "BOS/{FY}/",
    creditNotePrefix: "CN/{FY}/",
    sampleNumbers: ["TI/26-27/0001", "BOS/26-27/0001", "CN/26-27/0001"],
    enabled: true,
    version: 0,
    updatedAt: null,
    presets: PRESETS,
    rates: [0, 5, 12, 18, 28, 40],
    states: STATES,
  };
  const heads: GstFeeHead[] = [
    { feeHeadId: "fh-tuition", code: "TUITION", name: "Tuition fee", category: "tuition", status: "active", tax: null },
    { feeHeadId: "fh-hostel", code: "HOSTEL", name: "Hostel rent", category: "hostel", status: "active", tax: { sac: "996311", gstRate: "12", exempt: false, treatment: "taxable", taxNote: "", taxInclusive: false, version: 1 } },
  ];
  const parties = new Map<string, GstParty>();
  return {
    profile: async (inst) => ({ ...profile, institutionId: inst }),
    saveProfile: async (inst, input) => {
      const state = STATES.find((s) => s.code === (input.stateCode || input.gstin?.slice(0, 2)));
      profile = { ...profile, ...input, gstin: input.gstin?.trim().toUpperCase() || null, stateCode: state?.code ?? profile.stateCode, stateName: state?.name ?? null, institutionId: inst, configured: true, version: profile.version + 1, updatedAt: new Date().toISOString() } as GstProfile;
      return profile;
    },
    feeHeads: async () => heads.map((h) => ({ ...h })),
    saveFeeHead: async (_inst, id, input) => {
      const h = heads.find((x) => x.feeHeadId === id);
      if (!h) throw new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
      const rate = input.exempt ? "0" : input.gstRate;
      h.tax = { sac: input.sac, gstRate: rate, exempt: !!input.exempt, treatment: input.exempt ? "exempt" : rate === "0" ? "nil" : "taxable", taxNote: input.taxNote ?? "", taxInclusive: !!input.taxInclusive, version: (h.tax?.version ?? 0) + 1 };
      return { ...h };
    },
    clearFeeHead: async (_inst, id) => {
      const h = heads.find((x) => x.feeHeadId === id);
      if (h) h.tax = null;
    },
    party: async (id) => parties.get(id) ?? { studentAccountId: id, configured: false, stateCode: null, stateName: null, gstin: null, legalName: "", version: 0 },
    saveParty: async (id, input) => {
      const state = STATES.find((s) => s.code === (input.stateCode || input.gstin?.slice(0, 2))) ?? null;
      const p: GstParty = { studentAccountId: id, configured: true, stateCode: state?.code ?? null, stateName: state?.name ?? null, gstin: input.gstin?.trim().toUpperCase() || null, legalName: input.legalName ?? "", version: (parties.get(id)?.version ?? 0) + 1 };
      parties.set(id, p);
      return p;
    },
    chargeDocument: async () => null,
    summary: async (inst, p) => ({ institutionId: inst, gstin: profile.gstin, from: p.from, to: p.to, b2b: [], b2c: [], hsn: [], exempt: { intraNil: "0.00", intraExempt: "0.00", intraNonGst: "0.00", interNil: "0.00", interExempt: "0.00", interNonGst: "0.00" }, creditNotes: [], documents: [], net: zero }),
    summaryCsv: async (_inst, p) => ({ fileName: csvFileName(p), text: `GST summary,${profile.gstin ?? ""},${p.from},${p.to}\r\n` }),
  };
}

let fallback: GstApi | null = null;
export function defaultGstApi(): GstApi {
  fallback ??= USES_API ? createHttpGstApi() : createMockGstApi();
  return fallback;
}
