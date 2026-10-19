/** ERP_EXTENSIONS 3E: this device's printer config, support detection and the print-dialog fallback. */
import { ToastProvider } from "@sutra/ui";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createMockPrintingApi, PRINT_PATHS, slipQuery, type PrintingApi } from "./api";
import { PrintSlipButton } from "./PrintSlipButton";
import { base64Bytes, CONFIG_KEY, parsePrinterConfig, previewHtml, printerConfig, printerSupport, sendBytes, setPrinterConfig } from "./printer";
import { cleanSettings } from "./ReceiptPrintSettingsForm";

beforeEach(() => localStorage.clear());
afterEach(() => {
  document.querySelectorAll("iframe[data-print-preview]").forEach((f) => f.remove());
  vi.restoreAllMocks();
});

describe("3E the printer this device has", () => {
  it("reports no support in an environment with none, without throwing", () => {
    const s = printerSupport();
    expect(s).toEqual({ bluetooth: false, serial: false, usb: false, any: false });
  });

  it("remembers a choice on this device, and forgets it", () => {
    expect(printerConfig()).toBeNull();
    setPrinterConfig({ transport: "serial", name: "Counter printer", columns: 32, autoPrint: true, kickOnCash: true });
    expect(printerConfig()).toEqual({ transport: "serial", name: "Counter printer", columns: 32, autoPrint: true, kickOnCash: true });
    setPrinterConfig(null);
    expect(printerConfig()).toBeNull();
    expect(localStorage.getItem(CONFIG_KEY)).toBeNull();
  });

  it("refuses a stored value it doesn’t recognise", () => {
    localStorage.setItem(CONFIG_KEY, JSON.stringify({ transport: "telepathy" }));
    expect(printerConfig()).toBeNull();
    localStorage.setItem(CONFIG_KEY, "not json");
    expect(printerConfig()).toBeNull();
    expect(parsePrinterConfig("null")).toBeNull();
    expect(parsePrinterConfig("42")).toBeNull();
  });

  it("falls back to 80 mm (48 columns) when the width is nonsense", () => {
    setPrinterConfig({ transport: "usb", name: "x", columns: 999 as 48 });
    expect(printerConfig()!.columns).toBe(48);
    expect(parsePrinterConfig(JSON.stringify({ transport: "bluetooth", columns: "32" }))!.columns).toBe(48);
    expect(parsePrinterConfig(JSON.stringify({ transport: "bluetooth", name: "n".repeat(99) }))!.name).toHaveLength(60);
  });

  it("refuses to send with no printer, with a PrinterError", async () => {
    await expect(sendBytes(new Uint8Array([1]))).rejects.toMatchObject({ name: "PrinterError", cancelled: false });
  });

  it("decodes the server's base64 and escapes the preview page", () => {
    expect(Array.from(base64Bytes(btoa("\x1b@A")))).toEqual([0x1b, 0x40, 0x41]);
    expect(previewHtml("<b>Rs.5</b>", 32)).toContain("&lt;b&gt;Rs.5&lt;/b&gt;");
    expect(previewHtml("x", 32)).toContain("58mm");
  });
});

describe("3E API paths and settings", () => {
  it("keeps the paths under finance and builds the slip query", () => {
    for (const p of Object.values(PRINT_PATHS)) expect(p.startsWith("/api/v1/finance/")).toBe(true);
    expect(slipQuery({ columns: 32, kick: true })).toBe("columns=32&kick=true");
    expect(slipQuery({ columns: 48, duplicate: true })).toBe("columns=48&kick=false&copy=duplicate");
  });

  it("drops blank registrations and caps them at two", () => {
    const c = cleanSettings({ title: " ", address: " A ", footer: "", showAmountInWords: true, registrations: [{ label: "GSTIN", number: "27AAA" }, { label: "", number: "" }, { label: "PAN", number: "AAA" }, { label: "X", number: "1" }] });
    expect(c.title).toBe("");
    expect(c.address).toBe("A");
    expect(c.registrations).toEqual([{ label: "GSTIN", number: "27AAA" }, { label: "PAN", number: "AAA" }]);
  });
});

describe("3E PrintSlipButton", () => {
  it("falls back to the browser print dialog with the preview when no printer is configured", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    const base = createMockPrintingApi();
    const api: PrintingApi = { ...base, slip: vi.fn(base.slip) };
    render(
      <ToastProvider>
        <PrintSlipButton receiptId="r1" channel="cash" api={api} />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: /print/i }));
    await waitFor(() => expect(screen.getByText(/opened the print dialog/i)).toBeTruthy());
    expect(api.slip).toHaveBeenCalledWith("r1", { columns: 48, kick: false, duplicate: false });
    const frame = document.querySelector<HTMLIFrameElement>("iframe[data-print-preview]");
    expect(frame).not.toBeNull();
    expect(frame!.contentDocument!.body.textContent).toContain("RCT-000001");
  });

  it("asks for a duplicate copy at the device's width", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    localStorage.setItem(CONFIG_KEY, JSON.stringify({ transport: "telepathy", columns: 32 }));
    const base = createMockPrintingApi();
    const api: PrintingApi = { ...base, slip: vi.fn(base.slip) };
    render(
      <ToastProvider>
        <PrintSlipButton receiptId="r2" api={api} showDuplicate />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: /duplicate copy/i }));
    await waitFor(() => expect(api.slip).toHaveBeenCalledWith("r2", { columns: 48, kick: false, duplicate: true }));
  });
});
