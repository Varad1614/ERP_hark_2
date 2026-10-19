/**
 * ERP_EXTENSIONS 3E: "Print" (and "Duplicate copy") for a fee receipt. Fetches the server-built
 * slip at this device's paper width and sends it to the configured printer; without one, or when
 * it fails, the browser print dialog opens with the slip text instead.
 */
import { Printer } from "lucide-react";
import { Button, useToast } from "@sutra/ui";
import { useState } from "react";
import { problemText } from "../../institution/hooks";
import { defaultPrintingApi, type PrintingApi } from "./api";
import { outcomeText, printReceipt } from "./print";

export interface PrintSlipButtonProps {
  receiptId: string;
  /** Payment channel of the receipt (the drawer only opens for cash, server-side). */
  channel?: string;
  institutionId?: string;
  /** Show the "Duplicate copy" reprint next to Print. */
  showDuplicate?: boolean;
  size?: "sm" | "md";
  api?: PrintingApi;
}

export function PrintSlipButton({ receiptId, channel, showDuplicate = false, size = "sm", api = defaultPrintingApi() }: PrintSlipButtonProps) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const go = async (duplicate: boolean) => {
    setBusy(true);
    try {
      const { outcome } = await printReceipt(api, receiptId, { duplicate });
      toast(outcomeText(outcome, duplicate ? "Duplicate copy" : "Receipt"));
    } catch (e) {
      toast(problemText(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <span style={{ display: "inline-flex", gap: 6 }} data-print-slip={receiptId} data-channel={channel}>
      <Button size={size} variant="ghost" icon={<Printer strokeWidth={1.75} />} disabled={busy} data-print-receipt onClick={() => void go(false)}>
        Print
      </Button>
      {showDuplicate && (
        <Button size={size} variant="ghost" disabled={busy} data-print-duplicate onClick={() => void go(true)}>
          Duplicate copy
        </Button>
      )}
    </span>
  );
}
