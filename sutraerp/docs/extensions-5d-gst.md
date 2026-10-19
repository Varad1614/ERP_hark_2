# 5D — GST tax invoices, bills of supply and GST credit notes

Port of HeyPuter/everything `src/invoicing/src/lib/money.js` (`lineTotal`, `docTotals`),
`taxnote.js` (zero-tax notes and presets, `chargesUntaxed`) and `doc.js` (`formatDocNumber`) at
commit 06802a1, MIT (see `THIRD_PARTY_NOTICES.md`). Migration
`migrations/20270105000100_finance_gst.sql`.

The **charge stays the obligation**. A charge raised on a fee head with a tax setup keeps its
lines at the taxable value and gets one extra line per tax row on the system fee heads
`GST-CGST` / `GST-SGST` / `GST-IGST`, whose income accounts are the output-tax liability accounts
**2510 Output CGST payable**, **2520 Output SGST / UTGST payable**, **2530 Output IGST payable**
(seeded on first use). Publication therefore posts `Dr receivables · Cr income · Cr output tax`
through the existing posting path; there is no second ledger path for tax.

## Indian rules

- Education services by an educational institution to its students, faculty and staff are
  exempt (Notification 12/2017-Central Tax (Rate), Sl. No. 66, SAC 9992). Hostel, canteen,
  transport by a third party, uniforms / books, consultancy and rent may be taxable.
- Slabs: 0, 5, 12, 18, 28, 40 %. An exempt head has rate 0 and a note.
- Place of supply = the payer's state (`gst-details`), else the institution's state. Same state →
  **CGST + SGST** halves; different state → **IGST**.
- Treatments: `taxable` (rate > 0), `nil` (0 %, not exempt), `exempt`, `non_gst` (no setup).
- A charge with at least one taxable line → **Tax Invoice**; with only nil / exempt lines →
  **Bill of Supply**. A composition registration never collects tax and always issues bills of
  supply; an unregistered institution issues no GST documents.
- Document series per April–March financial year: defaults `TI/{FY}/`, `BOS/{FY}/`, `CN/{FY}/`
  (`{FY}` → `26-27`, also `{YYYY}`), number padded to 4 digits, whole number ≤ 16 characters,
  unique per institution and FY across every series. A charge's document is `pending` until the
  charge is published, then numbered and dated (`issued`).
- Rounding follows `docTotals`: each tax row is rounded to the paisa so printed rows add up;
  tax-inclusive heads carve the taxable value out once with the sum of the rates.
- GSTIN: 15 characters, state code prefix, checksum check character
  (`crates/kernel/src/india.rs` `gstin_valid` / `gstin_state`, shared with 3D receipts). The
  institution's GSTIN must belong to its profile state.

## Credit notes

A `credit_note` adjustment on a charge that has a tax invoice reverses tax **in proportion**
(credit ÷ document total): `Dr output tax · Dr income · Cr receivables`, a GST credit note with
its own CN series number that references the original invoice number and date. Reversing the
adjustment cancels the credit note (`cancelled`) and restores the tax.

## Tables (schema `finance`, RLS via `platform.apply_tenant_rls()`)

| table | what |
|---|---|
| `gst_profile` | one per institution: registration_type (regular / composition / unregistered), gstin, legal / trade name, address, state_code, default_tax_note, invoice / bill / credit-note prefixes, enabled, version |
| `fee_head_tax` | one per fee head: sac (4–8 digits), gst_rate, exempt, tax_note, tax_inclusive, version |
| `gst_party` | one per student account: state_code (place of supply), gstin (B2B), legal_name |
| `gst_series` | (institution, kind, fy_start) → next_value |
| `gst_document` | tax_invoice / bill_of_supply / credit_note; pending → issued → cancelled; charge_id or adjustment_id; supplier_state, place_of_supply, taxable_value, cgst, sgst, igst, total (= sum); original document of a credit note |
| `gst_document_line` | supplies on a document (never its tax lines): sac, treatment, rate, taxable value, tax |
| `gst_document_tax` | tax rows (`CGST @ 9%`) with the charge line that carries each |

## HTTP — tag `finance-gst`, ops `finance_gst_*`

| op | method + path | permission | body / response |
|---|---|---|---|
| `finance_gst_profile_get` | GET `/api/v1/finance/institutions/{id}/gst-profile` | finance.read | `GstProfileView` (defaults until saved; presets, rates, states, sample numbers) |
| `finance_gst_profile_put` | PUT same | finance.config.manage | `GstProfileInput` → `GstProfileView` |
| `finance_gst_fee_head_list` | GET `/api/v1/finance/institutions/{id}/gst-fee-heads` | finance.read | `GstFeeHead[]` (system GST heads left out; `tax` null when not set up) |
| `finance_gst_fee_head_put` | PUT `/api/v1/finance/institutions/{id}/gst-fee-heads/{feeHeadId}` | finance.config.manage | `GstFeeHeadTaxInput` → `GstFeeHead` |
| `finance_gst_fee_head_delete` | DELETE same | finance.config.manage | 204; the head goes back to non-GST |
| `finance_gst_party_get` | GET `/api/v1/finance/student-accounts/{id}/gst-details` | finance.read | `GstPartyView` |
| `finance_gst_party_put` | PUT same | charge.manage | `GstPartyInput` → `GstPartyView` |
| `finance_gst_charge_document` | GET `/api/v1/finance/charges/{id}/tax-document` | finance.read | `GstTaxDocument`; 404 when the charge has no GST document |
| `finance_gst_self_charge_document` | GET `/api/v1/finance/me/charges/{id}/tax-document` | finance.self | the student's own document |
| `finance_gst_document_get` | GET `/api/v1/finance/gst-documents/{id}` | finance.read | `GstTaxDocument` (also credit notes) |
| `finance_gst_document_list` | GET `/api/v1/finance/institutions/{id}/gst-documents?from&to` | finance.read | `GstDocumentItem[]`, newest first |
| `finance_gst_summary_get` | GET `/api/v1/finance/institutions/{id}/gst-summary?from&to` | finance.read | `GstSummaryView` |
| `finance_gst_summary_csv` | GET `/api/v1/finance/institutions/{id}/gst-summary/csv?from&to` | finance.read | `text/csv`, attachment `gst-summary-<from>-to-<to>.csv` |

`from` defaults to the 1st of this month and `to` to today (institution time).

```ts
GstProfileInput = { registrationType: "regular" | "composition" | "unregistered",
  gstin?: string | null, legalName: string, tradeName?: string, address?: string,
  stateCode?: string | null /* default: the GSTIN's state */,
  defaultTaxNote?: string /* "preset:<id>" or own words, ≤ 400 */,
  invoicePrefix?: string, billPrefix?: string, creditNotePrefix?: string, enabled?: boolean }
GstFeeHeadTaxInput = { sac: string, gstRate: "0"|"5"|"12"|"18"|"28"|"40", exempt?: boolean,
  taxNote?: string, taxInclusive?: boolean }
GstTaxDocument = { id, kind: "tax_invoice"|"bill_of_supply"|"credit_note", title, status,
  number, date, dateDisplay, financialYear, chargeId, chargeNumber, adjustmentNumber,
  supplier: {legalName, tradeName, gstin, address, stateCode, stateName},
  recipient: {name, studentNumber, gstin, stateCode, stateName},
  placeOfSupply, interState, reverseCharge,
  lines: {lineNo, description, sac, treatment, gstRate, taxableValue, cgst, sgst, igst, total}[],
  taxes: {component: "cgst"|"sgst"|"igst", label, rate, amount}[],
  taxableValue, cgst, sgst, igst, totalTax, total, totalDisplay, amountInWords, taxNote,
  original: {id, number, date} | null }
GstSummaryView = { institutionId, gstin, from, to, b2b[], b2c[] /* by place of supply + rate */,
  hsn[] /* SAC summary */, exempt: {intraNil, intraExempt, intraNonGst, interNil, interExempt,
  interNonGst}, creditNotes[] /* registered → cdnr, else cdnur */, documents[] /* issued ranges */,
  net: {taxableValue, cgst, sgst, igst, totalTax} }
```

Money is a decimal string. The CSV is one sheet: a title row (`GST summary`, GSTIN, from, to), a
header row, then one row per GSTR-1 entry with its section in the first column (`b2b`, `b2cs`,
`b2cs_inter`, `cdnr`, `cdnur`, `hsn`, `exemp_intra`, `exemp_inter`, `docs`); CRLF line ends;
cells are guarded against spreadsheet formula injection.

## Pure module and golden

`modules/finance/src/gst/rules.rs`: `doc_totals`, `line_total`, `tax_note_value`,
`tax_note_text`, `charges_untaxed`, `format_doc_number`, `fy_label`, `gst_taxes`, `plan_charge`,
`credit_reversal`, `summarize`, `summary_csv`. Golden `tools/golden/specs/gst-invoicing.mjs` →
`modules/finance/tests/golden/gst_invoicing.json` (215 cases), replayed by
`modules/finance/tests/golden_gst.rs`. DB test `apps/api/tests/db_e5d_gst.rs`.

## Web

`apps/web/app/features/finance/gst`: the **GST** tile in Admin → Finance → Fees & charges
(profile, fee-head tax table, summary for a period with CSV download) and the **GST** section
of the charge side panel (document number, tax breakup, amount in words, payer GST details).
Query keys start with `institution` so `useCommand` refreshes them.
