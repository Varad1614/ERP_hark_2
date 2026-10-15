/**
 * CSV cells for files users open in a spreadsheet (Part 1 · Stage 3 appsec, SEC-P1-APPSEC-006).
 *
 * Quoting: a cell with a comma, quote, CR or LF is wrapped in quotes (quotes doubled).
 * Formula injection (CWE-1236): a cell starting with `=`, `+`, `-`, `@`, TAB or CR is a formula to
 * Excel / LibreOffice / Sheets (`=HYPERLINK(...)`, `=cmd|' /C calc'!A0`), so such text is
 * prefixed with an apostrophe and read as text. Plain numbers (`-1250.50`, `+5`) stay numbers.
 */
const FORMULA_START = /^[=+\-@\t\r]/;
const PLAIN_NUMBER = /^[+-]?\d+(\.\d+)?$/;

export function csvCell(value: unknown): string {
  let v = value == null ? "" : String(value);
  if (FORMULA_START.test(v) && !PLAIN_NUMBER.test(v)) v = `'${v}`;
  return /[",\r\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v;
}

export function csvRow(values: unknown[]): string {
  return values.map(csvCell).join(",");
}
