/** ERP_EXTENSIONS 3D screen helpers: flag words, money, receipt downscaling and OCR prefill. */
import { formatInr } from "../../../finance";
import type { ExpenseSettings, ExpenseView, OcrFields, PaymentMethod, ReceiptContentType, ReceiptReading } from "./types";

export const PAYMENT_METHODS: PaymentMethod[] = ["Card", "Cash", "Bank transfer", "Mobile payment", "Other"];

/** Flags that stop a claim from being submitted. */
export const BLOCKING_FLAGS = ["receipt_missing", "too_old", "future_date", "foreign_currency"] as const;
export const isBlocking = (f: string) => (BLOCKING_FLAGS as readonly string[]).includes(f);

/** Plain-language chip text of a policy flag. */
export function flagLabel(flag: string, maxAgeDays?: number): string {
  switch (flag) {
    case "over_limit":
      return "Over the category limit";
    case "receipt_missing":
      return "Receipt needed";
    case "too_old":
      return maxAgeDays ? `Older than ${maxAgeDays} days` : "Too old to claim";
    case "future_date":
      return "Date in the future";
    case "uncategorised":
      return "No category";
    case "foreign_currency":
      return "Only rupee expenses can be claimed";
    default:
      return flag.replace(/_/g, " ");
  }
}

/** Why Submit is disabled, in words (empty when it can be submitted). */
export function submitBlockers(c: { canSubmit: boolean; lineCount: number; flags: string[]; status: string }, maxAgeDays?: number): string[] {
  if (c.canSubmit) return [];
  if (c.status !== "draft") return ["Only a draft claim can be submitted."];
  if (c.lineCount === 0) return ["Add at least one expense."];
  const blocking = [...new Set(c.flags.filter(isBlocking))].map((f) => flagLabel(f, maxAgeDays));
  return blocking.length ? blocking.map((b) => `Fix: ${b.toLowerCase()}.`) : ["The claim cannot be submitted yet."];
}

/** "1234567.5" → "₹12,34,567.50" (en-IN lakh grouping, the finance helper); blank/null → "—". */
export function inr(amount: string | number | null | undefined): string {
  if (amount === null || amount === undefined || amount === "") return "—";
  if (typeof amount === "number") return Number.isFinite(amount) ? formatInr(amount.toFixed(2)) : "—";
  return formatInr(amount);
}

/** Money of a line in its own currency (only rupees are formatted with ₹). */
export const money = (amount: string, currency: string) => (currency === "INR" ? inr(amount) : `${currency} ${amount}`);

/** "2026-10" → "Oct 26". */
export function monthLabel(ym: string): string {
  const [y, m] = ym.split("-").map(Number) as [number, number];
  return `${["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][m - 1] ?? ym} ${String(y % 100).padStart(2, "0")}`;
}

/** The OCR total of a line differs from what is claimed (null when no reading). */
export function ocrMismatch(line: Pick<ExpenseView, "amount" | "ocr">): { read: number; claimed: number } | null {
  const read = line.ocr?.total;
  if (typeof read !== "number" || !Number.isFinite(read)) return null;
  const claimed = Number(line.amount);
  return Math.abs(read - claimed) >= 0.005 ? { read, claimed } : null;
}

export const STATUS_LABEL: Record<string, string> = { draft: "Draft", submitted: "Submitted", approved: "Approved", reimbursed: "Reimbursed" };
export const statusKind = (s: string): "attention" | "info" | "done" | "neutral" => (s === "reimbursed" ? "done" : s === "approved" || s === "submitted" ? "info" : "neutral");

// ------------------------------------------------------------------ receipts

/** Largest decoded receipt the API accepts. */
export const MAX_RECEIPT_BYTES = 1.5 * 1024 * 1024;
export const MAX_EDGE = 1600;
export const JPEG_QUALITY = 0.8;

/** Size that fits the long edge within `max`, keeping the aspect ratio (never upscales). */
export function fitWithin(width: number, height: number, max = MAX_EDGE): { width: number; height: number } {
  const long = Math.max(width, height);
  if (long <= max || long === 0) return { width, height };
  const k = max / long;
  return { width: Math.max(1, Math.round(width * k)), height: Math.max(1, Math.round(height * k)) };
}

/** Decoded byte length of a base64 string. */
export function base64Bytes(b64: string): number {
  const s = b64.replace(/\s/g, "");
  const pad = s.endsWith("==") ? 2 : s.endsWith("=") ? 1 : 0;
  return Math.floor((s.length * 3) / 4) - pad;
}

function bytesToBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

const dataUrlBase64 = (url: string) => url.slice(url.indexOf(",") + 1);

/**
 * Turns a picked file into an upload: photos are drawn onto a canvas at most 1600 px on the long
 * edge and re-encoded as JPEG 0.8; PDFs go as they are. Throws when the result is over 1.5 MB.
 */
export async function prepareReceipt(file: File): Promise<{ fileName: string; contentType: ReceiptContentType; dataBase64: string }> {
  if (file.type === "application/pdf") {
    if (file.size > MAX_RECEIPT_BYTES) throw new Error("This PDF is larger than 1.5 MB. Save a smaller copy or photograph the receipt.");
    return { fileName: file.name, contentType: "application/pdf", dataBase64: bytesToBase64(new Uint8Array(await file.arrayBuffer())) };
  }
  if (!file.type.startsWith("image/")) throw new Error("Pick a photo (JPEG, PNG, WebP) or a PDF.");
  const bitmap = await createImageBitmap(file);
  const { width, height } = fitWithin(bitmap.width, bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("This browser cannot resize photos.");
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, width, height);
  ctx.drawImage(bitmap, 0, 0, width, height);
  bitmap.close?.();
  const dataBase64 = dataUrlBase64(canvas.toDataURL("image/jpeg", JPEG_QUALITY));
  if (base64Bytes(dataBase64) > MAX_RECEIPT_BYTES) throw new Error("This photo is still larger than 1.5 MB after resizing.");
  const fileName = file.name.replace(/\.[^.]+$/, "") + ".jpg";
  return { fileName, contentType: "image/jpeg", dataBase64 };
}

// ------------------------------------------------------------------ form

export interface ExpenseForm {
  spentOn: string;
  merchant: string;
  amount: string;
  currency: string;
  categoryId: string;
  paymentMethod: string;
  tax: string;
  gstin: string;
  notes: string;
  items: { name: string; amount: number | null }[];
  receiptId: string | null;
}

export const blankForm = (today: string): ExpenseForm => ({
  spentOn: today, merchant: "", amount: "", currency: "INR", categoryId: "", paymentMethod: "", tax: "", gstin: "", notes: "", items: [], receiptId: null,
});

const num = (v: number | null | undefined) => (typeof v === "number" && Number.isFinite(v) ? v.toFixed(2) : "");

/** Pre-fills the confirm form from what the receipt read (categories outside policy are dropped). */
export function formFromReading(reading: ReceiptReading | null | undefined, receiptId: string | null, today: string, settings?: Pick<ExpenseSettings, "categories">): ExpenseForm {
  const f: OcrFields | null | undefined = reading?.status === "read" ? reading.fields : null;
  const base = { ...blankForm(today), receiptId };
  if (!f) return { ...base, gstin: reading?.gstin ?? "" };
  const known = !settings || settings.categories.some((c) => c.id === f.categoryId);
  return {
    ...base,
    spentOn: f.date && /^\d{4}-\d{2}-\d{2}$/.test(f.date) && f.date <= today ? f.date : today,
    merchant: f.merchant ?? "",
    amount: num(f.total),
    currency: f.currency || "INR",
    categoryId: f.categoryId && known ? f.categoryId : "",
    paymentMethod: f.paymentMethod && (PAYMENT_METHODS as string[]).includes(f.paymentMethod) ? f.paymentMethod : "",
    tax: num(f.tax),
    gstin: reading?.gstin ?? "",
    notes: f.notes ?? (f.cardLast4 ? `Card ending ${f.cardLast4}` : ""),
    items: f.items ?? [],
  };
}

/** The confirm form of an existing expense. */
export const formFromExpense = (e: ExpenseView): ExpenseForm => ({
  spentOn: e.spentOn, merchant: e.merchant, amount: e.amount, currency: e.currency, categoryId: e.categoryId ?? "", paymentMethod: e.paymentMethod ?? "",
  tax: e.tax ?? "", gstin: e.gstin ?? "", notes: e.notes ?? "", items: e.items, receiptId: e.receiptId,
});

/** Low-confidence reading (< 0.5): ask the user to check harder. */
export const lowConfidence = (r: ReceiptReading | null | undefined) => r?.status === "read" && (r.fields?.confidence ?? 0) < 0.5;

/** The wire body of a confirmed form. */
export function inputOfForm(f: ExpenseForm) {
  const opt = (s: string) => (s.trim() === "" ? null : s.trim());
  return {
    spentOn: f.spentOn, merchant: f.merchant.trim(), amount: f.amount.trim(), currency: f.currency || "INR", categoryId: opt(f.categoryId),
    paymentMethod: opt(f.paymentMethod) as PaymentMethod | null, tax: opt(f.tax), gstin: opt(f.gstin)?.toUpperCase() ?? null, notes: opt(f.notes),
    items: f.items, receiptId: f.receiptId,
  };
}

/** Form can be saved: a merchant, a date not in the future and a positive amount. */
export const formReady = (f: ExpenseForm, today: string) => f.merchant.trim() !== "" && /^\d+(\.\d{1,2})?$/.test(f.amount.trim()) && Number(f.amount) > 0 && f.spentOn !== "" && f.spentOn <= today;
