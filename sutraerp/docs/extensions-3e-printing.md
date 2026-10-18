# 3E — Thermal receipt printing at the fee counter (ESC/POS)

Port of HeyPuter/everything `src/pos/src/lib/escpos.js` (+ the per-device printer
transport idea of `printer.js`, the doc model idea of `receipt-doc.js`) at commit 06802a1, MIT.

The **server** builds the slip (doc model, ESC/POS bytes, plain-text preview) so every copy says
the same thing; the **browser** only moves bytes to the printer on the counter (Web Serial,
WebUSB, Web Bluetooth). The printer choice is per device (localStorage), never server settings.

## Pure module — `modules/finance/src/printing/escpos.rs` (owner: escpos agent)

```rust
pub const PAPER_WIDTHS: [(usize, &str); 2] = [(32, "58 mm · 32 characters"), (48, "80 mm · 48 characters")];
pub fn encode_cp437(text: &str) -> Vec<u8>;           // NFC, transliterate, CP437, else b'?'
pub fn print_width(text: &str) -> usize;              // == encode_cp437(text).len()
pub fn wrap(text: &str, width: usize) -> Vec<String>;
pub struct EscposBuilder { .. }                       // new(columns), init/align/bold/big/text/line/feed/columns_line/wrapped/rule/cut/raw/build
#[derive(Serialize, Deserialize, ToSchema, Default)] #[serde(rename_all = "camelCase")]
pub struct SlipDoc {
    pub store_name: String, pub address: String, pub registrations: Vec<String>,
    pub voided: bool, pub kind: Option<String>, pub meta: Vec<String>,
    pub items: Vec<SlipItem>, pub totals: Vec<SlipRow>, pub payments: Vec<SlipRow>,
    pub note: String, pub reward: String, pub loyalty: String, pub footer: String,
}
pub struct SlipItem { pub qty: String, pub name: String, pub amount: String, pub subs: Vec<String> }
pub struct SlipRow  { pub label: String, pub value: String, #[serde(default)] pub strong: bool }
pub struct ReportHead { pub store_name: String, pub address: String, pub kind: String, pub meta: Vec<String>, pub footer: String }
pub struct ReportRow  { pub label: String, pub value: String, pub section: Option<String>, #[serde(default)] pub strong: bool }
pub fn escpos_receipt(doc: &SlipDoc, columns: usize, kick: bool) -> Vec<u8>;
pub fn escpos_report(head: &ReportHead, rows: &[ReportRow], columns: usize) -> Vec<u8>;
pub fn drawer_kick_bytes(connector: u8) -> Vec<u8>;
pub fn readable(bytes: &[u8]) -> String;              // preview: drop ESC/GS commands, CP437 -> Unicode
```

utoipa schema names are prefixed: `ReceiptSlipDoc`, `ReceiptSlipItem`, `ReceiptSlipRow`,
`CounterReportRow` (use `#[schema(as = ...)]`).

Indianisation (deliberate divergence from the JS, covered by Rust unit tests, kept out of the
golden inputs): `₹` prints as `Rs.` (JS: `INR`). Devanagari has no CP437 bytes and prints `?`.

Golden: `tools/golden/specs/escpos.mjs` → `modules/finance/tests/golden/escpos.json` (200 cases),
replayed by `modules/finance/tests/golden_escpos.rs`.

## Slip builder — `modules/finance/src/printing/slip.rs` (owner: main)

`fee_receipt_doc(&ReceiptView, &PrintSettings, issued_local: &str) -> SlipDoc`, Indian amount in
words (`Rupees One Lakh Twenty-Five Thousand Only`), counter day-close rows.

## HTTP — tag `finance-payments`, ops `finance_receipt_*` (owner: main)

| op | method + path | permission | body / response |
|---|---|---|---|
| `finance_receipt_print_settings_get` | GET `/api/v1/finance/institutions/{id}/receipt-print-settings` | finance.read | `ReceiptPrintSettings` |
| `finance_receipt_print_settings_put` | PUT same | finance.config.manage | `ReceiptPrintSettingsInput` → `ReceiptPrintSettings` |
| `finance_receipt_slip` | GET `/api/v1/finance/receipts/{id}/slip?columns=32\|48&kick=bool` | finance.read | `ReceiptSlip` |
| `finance_receipt_test_slip` | GET `/api/v1/finance/institutions/{id}/receipt-test-slip?columns=` | finance.read | `ReceiptSlip` (receiptId null) |
| `finance_receipt_counter_report` | GET `/api/v1/finance/institutions/{id}/counter-report?date=YYYY-MM-DD&columns=&mine=bool` | finance.read | `CounterReport` |

```ts
ReceiptPrintSettings = { institutionId, title: string, address: string,
  registrations: {label: string, number: string}[] /* ≤ 2, e.g. GSTIN, PAN */,
  footer: string, showAmountInWords: boolean, timezone: string /* default Asia/Kolkata */,
  updatedAt: string | null }
ReceiptPrintSettingsInput = same minus institutionId/updatedAt
ReceiptSlip = { receiptId: string | null, receiptNumber: string | null, columns: 32 | 48,
  doc: ReceiptSlipDoc, preview: string /* readable text */, escposBase64: string,
  kickDrawer: boolean /* true only when kick=true and the channel is cash */ }
CounterReport = { institutionId, date, mine: boolean, receiptCount: number, cancelledCount: number,
  total: string /* decimal */, totalDisplay: string /* ₹ lakh grouping */,
  byChannel: {channel: string, count: number, amount: string}[],
  rows: CounterReportRow[], preview: string, escposBase64: string }
```

`columns` other than 32/48 → 48. A cancelled receipt prints `*** VOID ***` and `CANCELLED`
meta; reprints carry `DUPLICATE` in `kind` when `?copy=duplicate`.

Migration `20270103000500_finance_receipt_printing.sql`: `finance.receipt_print_settings`
(tenant_id, institution_id PK pair, title, address, registrations jsonb, footer,
show_amount_in_words, timezone, updated_at, updated_by).

## Web (owner: web agent) — `apps/web/app/features/finance/printing/`

`printer.ts` (port of printer.js: support detection, localStorage config `sutra:printer:v1`,
Web Serial / WebUSB / Web Bluetooth write in chunks, PrinterError), `PrinterSetup` (transport,
paper width, auto-print, open drawer on cash, test print via `receipt-test-slip`),
`PrintSlipButton` (fetch slip → send bytes; falls back to the browser print dialog of the
`preview` text), print-settings form, counter day-close card. Mounted on the receipt view in
finance payments and the payments admin.
