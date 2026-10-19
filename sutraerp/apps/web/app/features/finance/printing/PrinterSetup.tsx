/**
 * ERP_EXTENSIONS 3E: this device's receipt printer (Admin → Finance → Collections → Counter
 * printing). The choice is per device, in localStorage — never server settings.
 */
import { Printer } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, FormGrid, FormSelect, KeyValue, useToast } from "@sutra/ui";
import { useState } from "react";
import { problemText } from "../../institution/hooks";
import { defaultPrintingApi, type PrintingApi } from "./api";
import { outcomeText, printBuilt } from "./print";
import { choosePrinter, PAPER_WIDTHS, printerConfig, printerSaid, printerSupport, setPrinterConfig, TRANSPORTS, type Columns, type PrinterConfig, type Transport } from "./printer";

export function PrinterSetup({ institutionId, api = defaultPrintingApi(), span = 6 }: { institutionId: string; api?: PrintingApi; span?: 4 | 6 | 12 }) {
  const toast = useToast();
  const support = printerSupport();
  const [config, setConfig] = useState<PrinterConfig | null>(() => printerConfig());
  const firstSupported = TRANSPORTS.find((t) => support[t.value])?.value ?? "serial";
  const [transport, setTransport] = useState<Transport>(config?.transport ?? firstSupported);
  const [busy, setBusy] = useState(false);
  // Width and toggles are remembered even before a printer is chosen (they shape the dialog print too).
  const [columns, setColumns] = useState<Columns>(config?.columns ?? 48);
  const [autoPrint, setAutoPrint] = useState(config?.autoPrint ?? false);
  const [kickOnCash, setKickOnCash] = useState(config?.kickOnCash ?? false);

  const patch = (p: Partial<PrinterConfig>) => {
    if (p.columns) setColumns(p.columns);
    if (p.autoPrint !== undefined) setAutoPrint(p.autoPrint);
    if (p.kickOnCash !== undefined) setKickOnCash(p.kickOnCash);
    if (config) setConfig(setPrinterConfig({ ...config, ...p }));
  };

  const choose = async () => {
    setBusy(true);
    try {
      const saved = await choosePrinter(transport);
      setConfig(saved ? setPrinterConfig({ ...saved, columns, autoPrint, kickOnCash }) : null);
      toast(`${saved?.name ?? "Printer"} set up on this device`);
    } catch (e) {
      if (!(e instanceof Error && "cancelled" in e && (e as { cancelled: boolean }).cancelled)) toast(printerSaid(e));
    } finally {
      setBusy(false);
    }
  };

  const testPrint = async () => {
    setBusy(true);
    try {
      const slip = await api.testSlip(institutionId, columns);
      toast(outcomeText(await printBuilt(slip, "Test print"), "Test print"));
    } catch (e) {
      toast(problemText(e));
    } finally {
      setBusy(false);
    }
  };

  const forget = () => {
    setConfig(setPrinterConfig(null));
    toast("Printer forgotten on this device");
  };

  return (
    <BentoTile span={span} title="Receipt printer on this device" data-slice="3E" data-printer-setup>
      {!support.any && (
        <p data-printer-unsupported style={{ margin: "0 0 12px", fontSize: 13, color: "var(--sb-mute)" }}>
          This browser can’t reach a printer directly — iPad and Safari have no Bluetooth, serial or USB access. Receipts print through the browser’s print dialog instead, which works with any printer the device knows.
        </p>
      )}
      {config ? (
        <KeyValue label="Printer" value={<span data-printer-name>{config.name || "Receipt printer"} · {TRANSPORTS.find((t) => t.value === config.transport)?.label}</span>} />
      ) : (
        <KeyValue label="Printer" value={<Badge kind="neutral">None — the print dialog is used</Badge>} />
      )}
      <FormGrid columns={2}>
        <FormSelect
          label="Connection"
          value={transport}
          onChange={(e) => setTransport(e.target.value as Transport)}
          data-printer-transport
          options={TRANSPORTS.map((t) => ({ value: t.value, label: support[t.value] ? t.label : `${t.label} (not in this browser)`, disabled: !support[t.value] }))}
        />
        <FormSelect
          label="Paper width"
          value={String(columns)}
          onChange={(e) => patch({ columns: Number(e.target.value) === 32 ? 32 : 48 })}
          data-printer-columns
          options={PAPER_WIDTHS.map((w) => ({ value: String(w.columns), label: w.label }))}
        />
      </FormGrid>
      <div style={{ display: "grid", gap: 6, margin: "8px 0 12px" }}>
        <Checkbox label="Print automatically after recording a cash receipt" checked={autoPrint} onChange={(e) => patch({ autoPrint: e.target.checked })} data-printer-autoprint />
        <Checkbox label="Open the cash drawer on cash receipts" description="Only for a drawer wired to the printer." checked={kickOnCash} onChange={(e) => patch({ kickOnCash: e.target.checked })} data-printer-kick />
      </div>
      {!config && (autoPrint || kickOnCash || columns !== 48) && (
        <p style={{ margin: "0 0 12px", fontSize: 12, color: "var(--sb-mute)" }}>These apply once a printer is chosen.</p>
      )}
      <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
        <Button size="sm" icon={<Printer strokeWidth={1.75} />} disabled={busy || !support[transport]} data-printer-choose onClick={() => void choose()}>
          {config ? "Choose another printer" : "Choose printer"}
        </Button>
        <Button size="sm" variant="ghost" disabled={busy} data-printer-test onClick={() => void testPrint()}>
          Test print
        </Button>
        {config && (
          <Button size="sm" variant="ghost" disabled={busy} data-printer-forget onClick={forget}>
            Forget printer
          </Button>
        )}
      </div>
    </BentoTile>
  );
}
