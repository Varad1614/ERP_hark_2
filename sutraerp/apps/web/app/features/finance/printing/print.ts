/**
 * 3E print flow shared by the slip button, auto-print and the day-close card: send the server's
 * ESC/POS bytes to this device's printer; with no printer (or when it fails) open the browser
 * print dialog with the slip's readable `preview` instead. Never throws for printer trouble —
 * only the slip request itself can fail.
 */
import type { PrintingApi } from "./api";
import { printerColumns, printerConfig, printerSaid, printPreview, sendEscpos, type Columns } from "./printer";
import type { ReceiptSlip } from "./types";

export type PrintOutcome = { via: "printer" } | { via: "dialog"; reason: "no-printer" | "printer-failed"; error?: string };

/** Print already-built bytes/preview (a slip, a test slip or a counter report). */
export async function printBuilt(built: { escposBase64: string; preview: string; columns?: Columns }, title: string): Promise<PrintOutcome> {
  const columns = built.columns ?? printerColumns();
  if (!printerConfig()) {
    printPreview(built.preview, columns, title);
    return { via: "dialog", reason: "no-printer" };
  }
  try {
    await sendEscpos(built.escposBase64);
    return { via: "printer" };
  } catch (e) {
    printPreview(built.preview, columns, title);
    return { via: "dialog", reason: "printer-failed", error: printerSaid(e) };
  }
}

/** Fetch a receipt's slip at this device's width and print it. */
export async function printReceipt(api: PrintingApi, receiptId: string, opts: { duplicate?: boolean } = {}): Promise<{ slip: ReceiptSlip; outcome: PrintOutcome }> {
  const config = printerConfig();
  const slip = await api.slip(receiptId, { columns: printerColumns(), kick: !!config?.kickOnCash, duplicate: opts.duplicate });
  const outcome = await printBuilt(slip, `Receipt ${slip.receiptNumber ?? ""}`.trim());
  return { slip, outcome };
}

/** Toast text of an outcome. */
export function outcomeText(o: PrintOutcome, what = "Receipt"): string {
  if (o.via === "printer") return `${what} sent to the printer`;
  if (o.reason === "printer-failed") return `${o.error ?? "The printer didn’t answer"} — opened the print dialog instead`;
  return "No printer on this device — opened the print dialog";
}

/**
 * Auto-print after recording a counter receipt, when this device asked for it (cash only —
 * the clerk hands the slip over with the change). Resolves to the toast text, or null when
 * nothing was printed.
 */
export async function autoPrintAfterRecording(api: PrintingApi, receipt: { id: string } | null | undefined, channel: string): Promise<string | null> {
  const config = printerConfig();
  if (!receipt || !config?.autoPrint || channel !== "cash") return null;
  try {
    const { outcome } = await printReceipt(api, receipt.id);
    return outcomeText(outcome);
  } catch {
    return "Receipt recorded — the slip couldn’t be fetched to print";
  }
}
