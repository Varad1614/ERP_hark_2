/**
 * ERP_EXTENSIONS 3E API layer (fee-counter printing): one interface, an HTTP implementation (CSRF
 * on writes, problem+json → `ApiProblem`) and an in-memory mock for mock mode. The 3E paths are
 * not in the generated OpenAPI client yet, so they are plain strings checked against
 * docs/extensions-3e-printing.md.
 */
import { ApiProblem, isProblem, isUnsafeMethod } from "@sutra/api-client";
import { getCsrfToken, USES_API } from "../../../lib/api";
import type { CounterReport, ReceiptPrintSettings, ReceiptPrintSettingsInput, ReceiptSlip } from "./types";

const B = "/api/v1/finance";
export const PRINT_PATHS = {
  settings: `${B}/institutions/{id}/receipt-print-settings`,
  slip: `${B}/receipts/{id}/slip`,
  testSlip: `${B}/institutions/{id}/receipt-test-slip`,
  counterReport: `${B}/institutions/{id}/counter-report`,
} as const;

export interface SlipOptions {
  columns: 32 | 48;
  kick?: boolean;
  duplicate?: boolean;
}

export interface PrintingApi {
  settings(institutionId: string): Promise<ReceiptPrintSettings>;
  saveSettings(institutionId: string, input: ReceiptPrintSettingsInput): Promise<ReceiptPrintSettings>;
  slip(receiptId: string, opts: SlipOptions): Promise<ReceiptSlip>;
  testSlip(institutionId: string, columns: 32 | 48): Promise<ReceiptSlip>;
  counterReport(institutionId: string, q: { date: string; columns: 32 | 48; mine: boolean }): Promise<CounterReport>;
}

const fill = (t: string, id: string) => t.replace("{id}", encodeURIComponent(id));
type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function slipQuery({ columns, kick, duplicate }: SlipOptions): string {
  const q = new URLSearchParams({ columns: String(columns), kick: String(!!kick) });
  if (duplicate) q.set("copy", "duplicate");
  return q.toString();
}

export function createHttpPrintingApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): PrintingApi {
  async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(body ?? {}) : undefined });
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
  return {
    settings: (inst) => request("GET", fill(PRINT_PATHS.settings, inst)),
    saveSettings: (inst, input) => request("PUT", fill(PRINT_PATHS.settings, inst), input),
    slip: (id, opts) => request("GET", `${fill(PRINT_PATHS.slip, id)}?${slipQuery(opts)}`),
    testSlip: (inst, columns) => request("GET", `${fill(PRINT_PATHS.testSlip, inst)}?columns=${columns}`),
    counterReport: (inst, q) => request("GET", `${fill(PRINT_PATHS.counterReport, inst)}?${new URLSearchParams({ date: q.date, columns: String(q.columns), mine: String(q.mine) }).toString()}`),
  };
}

// ------------------------------------------------------------------ mock mode

const b64 = (text: string) => {
  // ESC @ (init) + ASCII text + feed + GS V 0 (cut); the mock never needs more.
  const bytes = [0x1b, 0x40, ...Array.from(text, (c) => (c.charCodeAt(0) < 128 ? c.charCodeAt(0) : 0x3f)), 0x0a, 0x0a, 0x0a, 0x1d, 0x56, 0x00];
  return btoa(String.fromCharCode(...bytes));
};
const centre = (s: string, w: number) => " ".repeat(Math.max(0, Math.floor((w - s.length) / 2))) + s;
const pair = (l: string, r: string, w: number) => `${l}${" ".repeat(Math.max(1, w - l.length - r.length))}${r}`;

export function createMockPrintingApi(): PrintingApi {
  let saved: ReceiptPrintSettings = { institutionId: "", title: "", address: "", registrations: [], footer: "Thank you", showAmountInWords: true, updatedAt: null };
  const head = (w: number) => [centre(saved.title || "Sutra Demo School", w), ...(saved.address ? [centre(saved.address, w)] : []), ...saved.registrations.map((r) => centre(`${r.label}: ${r.number}`, w)), "-".repeat(w)];
  const slip = (id: string | null, columns: 32 | 48, kind: string | null): ReceiptSlip => {
    const lines = [...head(columns), ...(kind ? [centre(`*** ${kind} ***`, columns)] : []), pair("Receipt", id ? "RCT-000001" : "—", columns), pair("Tuition fee", "Rs.25,000.00", columns), "-".repeat(columns), pair("Total", "Rs.25,000.00", columns), ...(saved.footer ? [centre(saved.footer, columns)] : [])];
    const preview = lines.join("\n");
    return {
      receiptId: id,
      receiptNumber: id ? "RCT-000001" : null,
      columns,
      doc: { storeName: saved.title || "Sutra Demo School", address: saved.address, registrations: saved.registrations.map((r) => `${r.label}: ${r.number}`), voided: false, kind, meta: [], items: [], totals: [], payments: [], note: "", reward: "", loyalty: "", footer: saved.footer },
      preview,
      escposBase64: b64(preview),
      kickDrawer: false,
    };
  };
  return {
    settings: async (inst) => ({ ...saved, institutionId: inst }),
    saveSettings: async (inst, input) => {
      saved = { ...input, registrations: input.registrations.slice(0, 2), institutionId: inst, updatedAt: new Date().toISOString() };
      return saved;
    },
    slip: async (id, o) => slip(id, o.columns, o.duplicate ? "DUPLICATE" : null),
    testSlip: async (_inst, columns) => slip(null, columns, "TEST PRINT"),
    counterReport: async (inst, q) => {
      const preview = [...head(q.columns), centre("COUNTER DAY CLOSE", q.columns), pair("Date", q.date, q.columns), pair("Receipts", "1", q.columns), pair("Cash", "Rs.25,000.00", q.columns), pair("Total", "Rs.25,000.00", q.columns)].join("\n");
      return { institutionId: inst, date: q.date, mine: q.mine, receiptCount: 1, cancelledCount: 0, total: "25000.00", totalDisplay: "₹25,000.00", byChannel: [{ channel: "cash", count: 1, amount: "25000.00" }], rows: [], preview, escposBase64: b64(preview) };
    },
  };
}

let fallback: PrintingApi | null = null;
export function defaultPrintingApi(): PrintingApi {
  fallback ??= USES_API ? createHttpPrintingApi() : createMockPrintingApi();
  return fallback;
}
