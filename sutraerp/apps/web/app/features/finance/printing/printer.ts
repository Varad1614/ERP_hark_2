/**
 * ERP_EXTENSIONS 3E — talking to the thermal printer on the fee counter.
 *
 * Port of the transport layer of HeyPuter/everything `src/pos/src/lib/printer.js` at commit
 * 06802a1 (MIT). The server builds the slip (ESC/POS bytes + readable preview, see
 * docs/extensions-3e-printing.md); this module only moves bytes to the printer.
 *
 * A receipt printer is a property of *this device*, not of the institution: two counters have one
 * printer each and a laptop in the principal's office has none. So the choice lives in
 * localStorage (`sutra:printer:v1`), never in server settings.
 *
 * Browser hardware access is uneven and an iPad / Safari reaches none of this. Every function is
 * honest about that: `printerSupport()` says what this browser can reach, nothing throws on a
 * device without support, and the browser's own print dialog stays the fallback everywhere.
 */

export const CONFIG_KEY = "sutra:printer:v1";

export type Transport = "bluetooth" | "serial" | "usb";
export type Columns = 32 | 48;

export const TRANSPORTS: { value: Transport; label: string }[] = [
  { value: "bluetooth", label: "Bluetooth" },
  { value: "serial", label: "USB / serial" },
  { value: "usb", label: "USB (direct)" },
];

export const PAPER_WIDTHS: { columns: Columns; label: string }[] = [
  { columns: 32, label: "58 mm · 32 characters" },
  { columns: 48, label: "80 mm · 48 characters" },
];

export interface PrinterConfig {
  transport: Transport;
  name: string;
  columns: Columns;
  autoPrint: boolean;
  kickOnCash: boolean;
}

export interface PrinterSupport {
  bluetooth: boolean;
  serial: boolean;
  usb: boolean;
  any: boolean;
}

/**
 * GATT services the common Bluetooth thermal printers expose a writable characteristic on.
 * Requested as optional services; the write target is discovered rather than assumed, since the
 * pairing of service and characteristic differs between the cheap clones.
 */
export const BT_SERVICES: (number | string)[] = [
  0x18f0, // common on ESC/POS BLE printers
  0xff00,
  0xffe0,
  "000018f0-0000-1000-8000-00805f9b34fb",
  "0000ff00-0000-1000-8000-00805f9b34fb",
  "49535343-fe7d-4ae5-8fa9-9fafd205e455", // Microchip transparent UART
];

/** BLE writes are capped per packet; 180 bytes is inside every implementation's MTU. */
export const BT_CHUNK = 180;
export const SERIAL_BAUD = 9600;

// ------------------------------------------------------------------ local typings of the device APIs
// (Web Bluetooth / Web Serial / WebUSB are not in lib.dom; typed here, no new dependencies.)

interface BtCharacteristic {
  properties: { write: boolean; writeWithoutResponse: boolean };
  writeValue(data: BufferSource): Promise<void>;
  writeValueWithResponse?(data: BufferSource): Promise<void>;
  writeValueWithoutResponse?(data: BufferSource): Promise<void>;
}
interface BtService {
  getCharacteristics(): Promise<BtCharacteristic[]>;
}
interface BtServer {
  getPrimaryServices(): Promise<BtService[]>;
}
interface BtDevice {
  name?: string;
  gatt?: { connect(): Promise<BtServer>; disconnect(): void };
}
interface BluetoothApi {
  requestDevice(opts: { acceptAllDevices: boolean; optionalServices: (number | string)[] }): Promise<BtDevice>;
  getDevices?(): Promise<BtDevice[]>;
}
interface SerialPortLike {
  open(opts: { baudRate: number }): Promise<void>;
  close(): Promise<void>;
  writable: WritableStream<Uint8Array> | null;
}
interface SerialApi {
  requestPort(): Promise<SerialPortLike>;
  getPorts(): Promise<SerialPortLike[]>;
}
interface UsbEndpoint {
  endpointNumber: number;
  direction: "in" | "out";
  type?: string;
}
interface UsbAlternate {
  interfaceClass: number;
  endpoints: UsbEndpoint[];
}
interface UsbInterface {
  interfaceNumber: number;
  alternates: UsbAlternate[];
}
interface UsbDevice {
  productName?: string;
  configuration: { interfaces: UsbInterface[] } | null;
  open(): Promise<void>;
  close(): Promise<void>;
  selectConfiguration(n: number): Promise<void>;
  claimInterface(n: number): Promise<void>;
  releaseInterface(n: number): Promise<void>;
  transferOut(endpoint: number, data: BufferSource): Promise<unknown>;
}
interface UsbApi {
  requestDevice(opts: { filters: { classCode: number }[] }): Promise<UsbDevice>;
  getDevices(): Promise<UsbDevice[]>;
}
interface DeviceNavigator {
  bluetooth?: BluetoothApi;
  serial?: SerialApi;
  usb?: UsbApi;
}

const nav = (): DeviceNavigator => (typeof navigator === "undefined" ? {} : (navigator as unknown as DeviceNavigator));

/** What this browser, on this device, can actually reach. Never throws. */
export function printerSupport(): PrinterSupport {
  // Every one of these needs a secure context; without one the API objects are simply absent,
  // which reads as "not supported" — the honest answer for the device at the counter.
  let n: DeviceNavigator = {};
  try {
    n = nav();
  } catch {
    n = {};
  }
  const bluetooth = typeof n.bluetooth?.requestDevice === "function";
  const serial = typeof n.serial?.requestPort === "function";
  const usb = typeof n.usb?.requestDevice === "function";
  return { bluetooth, serial, usb, any: bluetooth || serial || usb };
}

const isTransport = (v: unknown): v is Transport => TRANSPORTS.some((t) => t.value === v);
const isColumns = (v: unknown): v is Columns => PAPER_WIDTHS.some((w) => w.columns === v);

/** Parse a stored value; nonsense → null, an unknown width → 48 (80 mm). */
export function parsePrinterConfig(raw: string | null): PrinterConfig | null {
  if (!raw) return null;
  try {
    const saved = JSON.parse(raw) as Partial<Record<keyof PrinterConfig, unknown>> | null;
    if (!saved || typeof saved !== "object" || !isTransport(saved.transport)) return null;
    return {
      transport: saved.transport,
      name: String(saved.name ?? "").slice(0, 60),
      columns: isColumns(saved.columns) ? saved.columns : 48,
      autoPrint: !!saved.autoPrint,
      kickOnCash: !!saved.kickOnCash,
    };
  } catch {
    return null;
  }
}

/** The saved choice, or null. Never throws: storage can be unavailable. */
export function printerConfig(): PrinterConfig | null {
  try {
    return parsePrinterConfig(localStorage.getItem(CONFIG_KEY));
  } catch {
    return null;
  }
}

/** The paper width to ask the server for: the configured one, else 80 mm. */
export const printerColumns = (): Columns => printerConfig()?.columns ?? 48;

/** Save (or, with null, forget) this device's printer. */
export function setPrinterConfig(config: Partial<PrinterConfig> | null): PrinterConfig | null {
  try {
    if (!config) localStorage.removeItem(CONFIG_KEY);
    else localStorage.setItem(CONFIG_KEY, JSON.stringify(config));
  } catch {
    /* the printer is then simply not remembered */
  }
  // A forgotten printer must not keep a live connection behind it.
  if (!config) disconnectPrinter();
  return printerConfig();
}

/** True when this device has a printer set up to print to. */
export const hasPrinter = () => !!printerConfig();

// ------------------------------------------------------------------ errors

/** A message the counter clerk can act on, rather than a DOMException. */
export class PrinterError extends Error {
  /** True when the *user* dismissed the browser's device picker. */
  readonly cancelled: boolean;
  constructor(message: string, { cancelled = false }: { cancelled?: boolean } = {}) {
    super(message);
    this.name = "PrinterError";
    this.cancelled = cancelled;
  }
}

/**
 * "The printer didn’t answer" — a printer failure as the first half of a sentence, so callers can
 * add what they did instead without two full stops colliding.
 */
export function printerSaid(err: unknown): string {
  const m = err instanceof Error && err.message ? err.message : "The printer didn’t answer";
  return m.replace(/\.$/, "");
}

const wasCancelled = (err: unknown) => {
  const e = err as { name?: string; message?: string } | null;
  return e?.name === "NotFoundError" || /cancel|dismiss/i.test(e?.message ?? "");
};

// ------------------------------------------------------------------ the live connection

interface Held {
  transport: Transport;
  name: string;
  /** Max bytes per write; 0 = the whole job at once. */
  chunk: number;
  write(bytes: Uint8Array): Promise<unknown>;
  close(): Promise<void>;
}

/**
 * One connection per device, kept open between jobs: a counter prints a receipt every minute and
 * re-pairing each time is slow and (on Bluetooth) a permission prompt.
 */
let live: Held | null = null;

export function disconnectPrinter() {
  const held = live;
  live = null;
  held?.close().catch(() => {});
}

const NEEDS_CHOOSING = "The printer needs to be chosen again on this device.";
const buf = (b: Uint8Array): BufferSource => b as unknown as BufferSource;

async function openBluetooth({ pick, name }: { pick: boolean; name: string }): Promise<Held> {
  const bt = nav().bluetooth!;
  let device: BtDevice | null = null;
  if (!pick && typeof bt.getDevices === "function") {
    // Already granted on this device: reconnect without a prompt.
    const known = await bt.getDevices().catch(() => [] as BtDevice[]);
    device = known.find((d) => !name || d.name === name) ?? null;
  }
  if (!device) {
    if (!pick) throw new PrinterError(NEEDS_CHOOSING);
    device = await bt.requestDevice({ acceptAllDevices: true, optionalServices: BT_SERVICES });
  }
  if (!device.gatt) throw new PrinterError("That Bluetooth device has nothing this app can print to.");
  const server = await device.gatt.connect();
  const services = await server.getPrimaryServices();
  for (const service of services) {
    const chars = await service.getCharacteristics().catch(() => [] as BtCharacteristic[]);
    const w = chars.find((c) => c.properties.write || c.properties.writeWithoutResponse);
    if (!w) continue;
    const dev = device;
    return {
      transport: "bluetooth",
      name: dev.name || "Bluetooth printer",
      chunk: BT_CHUNK,
      write: (bytes) =>
        w.properties.write
          ? (w.writeValueWithResponse?.(buf(bytes)) ?? w.writeValue(buf(bytes)))
          : (w.writeValueWithoutResponse?.(buf(bytes)) ?? w.writeValue(buf(bytes))),
      close: async () => dev.gatt?.disconnect(),
    };
  }
  throw new PrinterError("That Bluetooth device has nothing this app can print to.");
}

async function openSerial({ pick }: { pick: boolean }): Promise<Held> {
  const serial = nav().serial!;
  let port: SerialPortLike | null;
  if (!pick) {
    const known = await serial.getPorts().catch(() => [] as SerialPortLike[]);
    port = known[0] ?? null;
    if (!port) throw new PrinterError(NEEDS_CHOOSING);
  } else {
    port = await serial.requestPort();
  }
  await port.open({ baudRate: SERIAL_BAUD });
  if (!port.writable) throw new PrinterError("That port can’t be written to.");
  const writer = port.writable.getWriter();
  const p = port;
  return {
    transport: "serial",
    name: "Serial printer",
    chunk: 0,
    write: (bytes) => writer.write(bytes),
    close: async () => {
      try {
        await writer.close();
      } finally {
        await p.close().catch(() => {});
      }
    },
  };
}

/** Class 7 is the USB printer class — what a receipt printer presents. */
const USB_PRINTER_CLASS = 7;

async function openUsb({ pick }: { pick: boolean }): Promise<Held> {
  const usb = nav().usb!;
  let device: UsbDevice | null;
  if (!pick) {
    const known = await usb.getDevices().catch(() => [] as UsbDevice[]);
    device = known[0] ?? null;
    if (!device) throw new PrinterError(NEEDS_CHOOSING);
  } else {
    device = await usb.requestDevice({ filters: [{ classCode: USB_PRINTER_CLASS }] });
  }
  await device.open();
  if (!device.configuration) await device.selectConfiguration(1);
  const iface = device.configuration?.interfaces.find((i) => i.alternates.some((a) => a.interfaceClass === USB_PRINTER_CLASS));
  if (!iface) throw new PrinterError("That USB device isn’t a printer this app can use.");
  await device.claimInterface(iface.interfaceNumber);
  const alternate = iface.alternates.find((a) => a.interfaceClass === USB_PRINTER_CLASS)!;
  // The bulk OUT endpoint is where the job goes.
  const endpoint = alternate.endpoints.find((e) => e.direction === "out" && (!e.type || e.type === "bulk")) ?? alternate.endpoints.find((e) => e.direction === "out");
  if (!endpoint) throw new PrinterError("That printer has no channel to send to.");
  const dev = device;
  return {
    transport: "usb",
    name: dev.productName || "USB printer",
    chunk: 0,
    write: (bytes) => dev.transferOut(endpoint.endpointNumber, buf(bytes)),
    close: async () => {
      await dev.releaseInterface(iface.interfaceNumber).catch(() => {});
      await dev.close().catch(() => {});
    },
  };
}

const OPENERS: Record<Transport, (o: { pick: boolean; name: string }) => Promise<Held>> = { bluetooth: openBluetooth, serial: openSerial, usb: openUsb };

/**
 * A connection, reusing the open one. `pick: true` shows the browser's device picker, which only
 * a user gesture may do, so it is only passed from the setup button that says so.
 */
async function connection({ transport, name = "", pick = false }: { transport: Transport; name?: string; pick?: boolean }): Promise<Held> {
  if (live && !pick && live.transport === transport) return live;
  if (pick) disconnectPrinter();
  const open = OPENERS[transport];
  if (!open) throw new PrinterError("This device can’t reach that kind of printer.");
  if (!printerSupport()[transport]) throw new PrinterError("This browser can’t reach that kind of printer.");
  try {
    live = await open({ pick, name });
  } catch (err) {
    live = null;
    if (err instanceof PrinterError) throw err;
    if (wasCancelled(err)) throw new PrinterError("No printer chosen.", { cancelled: true });
    console.error(err);
    throw new PrinterError("Couldn’t reach the printer. Check it’s on and paired.");
  }
  return live;
}

/** Choose a printer: the one moment the browser shows its picker. Keeps the other settings. */
export async function choosePrinter(transport: Transport): Promise<PrinterConfig | null> {
  const held = await connection({ transport, pick: true });
  const before = printerConfig();
  return setPrinterConfig({
    transport: held.transport,
    name: held.name,
    columns: before?.columns ?? 48,
    autoPrint: before?.autoPrint ?? false,
    kickOnCash: before?.kickOnCash ?? false,
  });
}

/** Some transports cap a write; the rest take the whole slip at once. */
async function writeAll(held: Held, bytes: Uint8Array) {
  if (!held.chunk) {
    await held.write(bytes);
    return;
  }
  for (let at = 0; at < bytes.length; at += held.chunk) await held.write(bytes.slice(at, at + held.chunk));
}

/**
 * Send bytes to the configured printer. A dropped connection is retried once from scratch: a
 * Bluetooth printer that slept between students is the normal case, not an error to show.
 */
export async function sendBytes(bytes: Uint8Array): Promise<void> {
  const config = printerConfig();
  if (!config) throw new PrinterError("No printer is set up on this device.");
  let held: Held;
  try {
    held = await connection({ transport: config.transport, name: config.name });
  } catch {
    disconnectPrinter();
    held = await connection({ transport: config.transport, name: config.name });
  }
  try {
    await writeAll(held, bytes);
  } catch (err) {
    // One retry on a fresh connection, then it is a real failure.
    disconnectPrinter();
    const again = await connection({ transport: config.transport, name: config.name });
    try {
      await writeAll(again, bytes);
    } catch {
      console.error(err);
      throw new PrinterError("The printer didn’t take the job. Check paper and power.");
    }
  }
}

/** Decode the server's `escposBase64` into bytes. */
export function base64Bytes(b64: string): Uint8Array {
  const bin = atob(b64.replace(/\s+/g, ""));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** Send a server-built slip (base64 ESC/POS). */
export const sendEscpos = (b64: string) => sendBytes(base64Bytes(b64));

// ------------------------------------------------------------------ browser print fallback

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

/** The HTML of a narrow monospace page carrying the slip's readable text. */
export function previewHtml(preview: string, columns: Columns = 48, title = "Receipt"): string {
  const mm = columns === 32 ? 58 : 80;
  return `<!doctype html><html><head><meta charset="utf-8"><title>${esc(title)}</title><style>
@page { size: ${mm}mm auto; margin: 3mm; }
html, body { margin: 0; padding: 0; background: #fff; color: #000; }
pre { font-family: "Courier New", ui-monospace, monospace; font-size: ${columns === 32 ? 10 : 11}px; line-height: 1.25; white-space: pre-wrap; word-break: break-all; width: ${columns}ch; margin: 0 auto; }
</style></head><body><pre>${esc(preview)}</pre></body></html>`;
}

/**
 * Open the browser's print dialog with the slip text, from a hidden iframe (no pop-up blocker).
 * Returns false where there is no document (SSR / tests without a DOM).
 */
export function printPreview(preview: string, columns: Columns = 48, title = "Receipt"): boolean {
  if (typeof document === "undefined") return false;
  const frame = document.createElement("iframe");
  frame.setAttribute("aria-hidden", "true");
  frame.setAttribute("data-print-preview", "");
  frame.style.cssText = "position:fixed;right:0;bottom:0;width:0;height:0;border:0;visibility:hidden";
  document.body.appendChild(frame);
  const win = frame.contentWindow;
  const doc = win?.document;
  if (!win || !doc) {
    frame.remove();
    return false;
  }
  doc.open();
  doc.write(previewHtml(preview, columns, title));
  doc.close();
  const cleanup = () => setTimeout(() => frame.remove(), 1000);
  try {
    win.addEventListener?.("afterprint", cleanup);
    win.focus?.();
    win.print?.();
  } catch {
    /* jsdom and locked-down browsers: the frame still carries the text */
  }
  setTimeout(() => frame.remove(), 60_000);
  return true;
}
