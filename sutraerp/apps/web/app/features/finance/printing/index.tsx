/**
 * ERP_EXTENSIONS 3E — fee-counter thermal receipt printing (docs/extensions-3e-printing.md).
 * `CounterPrintingSection` is the "Counter printing" block of Admin → Finance → Collections.
 */
import { CounterDayCloseCard } from "./CounterDayCloseCard";
import { PrinterSetup } from "./PrinterSetup";
import { ReceiptPrintSettingsForm } from "./ReceiptPrintSettingsForm";

export { CounterDayCloseCard } from "./CounterDayCloseCard";
export { PrinterSetup } from "./PrinterSetup";
export { PrintSlipButton } from "./PrintSlipButton";
export { ReceiptPrintSettingsForm } from "./ReceiptPrintSettingsForm";
export { autoPrintAfterRecording, printReceipt } from "./print";
export { defaultPrintingApi, type PrintingApi } from "./api";

export function CounterPrintingSection({ institutionId, institutionName }: { institutionId: string; institutionName?: string }) {
  return (
    <>
      <CounterDayCloseCard institutionId={institutionId} />
      <PrinterSetup institutionId={institutionId} />
      <ReceiptPrintSettingsForm institutionId={institutionId} institutionName={institutionName} />
    </>
  );
}
