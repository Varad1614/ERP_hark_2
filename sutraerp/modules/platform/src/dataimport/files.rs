//! Import / export file formats (Part 2 · S4-import): CSV (RFC 4180, own reader and writer) and
//! XLSX (read with `calamine`, written with `rust_xlsxwriter`).
//!
//! **Formula injection** (SEC-P1-APPSEC-006, OWASP "CSV Injection"): every exported cell that
//! starts with `=`, `+`, `-`, `@`, TAB or CR is prefixed with a single quote, so a spreadsheet
//! never evaluates it — the same rule as the web's `csvCell` (`apps/web/app/lib/csv.ts`). XLSX
//! cells are always written as *strings* (never formulas) and get the same prefix. On import a
//! leading `'` in front of one of those characters is removed again, so an export re-imports
//! unchanged (round trip).

use std::io::Cursor;
use sutra_kernel::error::{FieldViolation, KernelError};

/// Upper bound of data rows in one file (bigger migrations are split into several files).
pub const MAX_ROWS: usize = 20_000;
/// Upper bound of columns in one file.
pub const MAX_COLUMNS: usize = 200;

/// File format of an upload / export.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FileFormat {
    Csv,
    Xlsx,
}

impl FileFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Xlsx => "xlsx",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "csv" => Some(Self::Csv),
            "xlsx" => Some(Self::Xlsx),
            _ => None,
        }
    }

    /// From the file name's extension (`.csv`, `.xlsx`; case-insensitive).
    pub fn from_file_name(name: &str) -> Option<Self> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".csv") {
            Some(Self::Csv)
        } else if lower.ends_with(".xlsx") {
            Some(Self::Xlsx)
        } else {
            None
        }
    }

    pub fn content_type(self) -> &'static str {
        match self {
            Self::Csv => "text/csv; charset=utf-8",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        }
    }
}

/// A parsed file: header cells and data rows (1-based source line, cells).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<(u32, Vec<String>)>,
}

fn bad(msg: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new("file", "unreadable", msg.into())])
}

fn xlsx_err(e: rust_xlsxwriter::XlsxError) -> KernelError {
    KernelError::internal_msg(format!("xlsx: {e}"))
}

/// Removes the export-side formula guard (`'=x` → `=x`) and surrounding whitespace.
pub fn unguard(cell: &str) -> String {
    let t = cell.trim();
    let mut chars = t.chars();
    if chars.next() == Some('\'') {
        if let Some(c) = chars.next() {
            if matches!(c, '=' | '+' | '-' | '@' | '\t' | '\r') {
                return t[1..].to_owned();
            }
        }
    }
    t.to_owned()
}

/// Neutralises a cell that a spreadsheet would evaluate as a formula.
pub fn guard(cell: &str) -> String {
    match cell.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{cell}"),
        _ => cell.to_owned(),
    }
}

/// One CSV field: guarded, quoted when it holds a separator, quote or line break.
pub fn csv_cell(cell: &str) -> String {
    let g = guard(cell);
    if g.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", g.replace('"', "\"\""))
    } else {
        g
    }
}

fn csv_line(cells: &[String]) -> String {
    cells
        .iter()
        .map(|c| csv_cell(c))
        .collect::<Vec<_>>()
        .join(",")
}

/// A CSV document (CRLF line ends, UTF-8 with BOM so spreadsheets detect the encoding).
pub fn write_csv(headers: &[String], rows: &[Vec<String>]) -> Vec<u8> {
    let mut out = String::from("\u{feff}");
    out.push_str(&csv_line(headers));
    out.push_str("\r\n");
    for r in rows {
        out.push_str(&csv_line(r));
        out.push_str("\r\n");
    }
    out.into_bytes()
}

/// An XLSX workbook with one sheet: bold header row, every cell a guarded string.
pub fn write_xlsx(
    sheet: &str,
    headers: &[String],
    rows: &[Vec<String>],
) -> Result<Vec<u8>, KernelError> {
    use rust_xlsxwriter::{Format, Workbook};
    let mut wb = Workbook::new();
    let bold = Format::new().set_bold();
    let name: String = sheet
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(31)
        .collect();
    let ws = wb.add_worksheet();
    ws.set_name(if name.is_empty() {
        "data".to_owned()
    } else {
        name
    })
    .map_err(xlsx_err)?;
    for (c, h) in headers.iter().enumerate() {
        let col = u16::try_from(c).map_err(|_| bad("too many columns"))?;
        ws.write_string_with_format(0, col, guard(h), &bold)
            .map_err(xlsx_err)?;
        ws.set_column_width(col, 18).map_err(xlsx_err)?;
    }
    for (r, cells) in rows.iter().enumerate() {
        let row = u32::try_from(r + 1).map_err(|_| bad("too many rows"))?;
        for (c, v) in cells.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let col = u16::try_from(c).map_err(|_| bad("too many columns"))?;
            ws.write_string(row, col, guard(v)).map_err(xlsx_err)?;
        }
    }
    wb.save_to_buffer().map_err(xlsx_err)
}

/// Parses an upload.
pub fn read(format: FileFormat, bytes: &[u8]) -> Result<Table, KernelError> {
    let t = match format {
        FileFormat::Csv => read_csv(bytes)?,
        FileFormat::Xlsx => read_xlsx(bytes)?,
    };
    if t.headers.iter().all(String::is_empty) {
        return Err(bad("the first row must hold the column headers"));
    }
    if t.headers.len() > MAX_COLUMNS {
        return Err(bad(format!("at most {MAX_COLUMNS} columns")));
    }
    let mut seen = std::collections::HashSet::new();
    for h in &t.headers {
        if !h.is_empty() && !seen.insert(h.to_lowercase()) {
            return Err(bad(format!("column header `{h}` appears twice")));
        }
    }
    if t.rows.is_empty() {
        return Err(bad("the file has no data rows"));
    }
    if t.rows.len() > MAX_ROWS {
        return Err(bad(format!(
            "at most {MAX_ROWS} data rows per file; split the migration into several files"
        )));
    }
    Ok(t)
}

/// RFC 4180 reader: `,` separator, `"` quoting with `""` escapes, CRLF / LF line ends, UTF-8
/// (a BOM is skipped). Blank lines are ignored; line numbers count physical lines.
pub fn read_csv(bytes: &[u8]) -> Result<Table, KernelError> {
    let text = std::str::from_utf8(bytes).map_err(|_| bad("CSV files must be UTF-8 encoded"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records: Vec<(u32, Vec<String>)> = Vec::new();
    let mut field = String::new();
    let mut record: Vec<String> = Vec::new();
    let mut in_quotes = false;
    let mut line: u32 = 1;
    let mut record_line: u32 = 1;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                '"' => in_quotes = false,
                '\n' => {
                    line += 1;
                    field.push('\n');
                }
                _ => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                in_quotes = true;
                any = true;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                any = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                record.push(std::mem::take(&mut field));
                if any || record.iter().any(|f| !f.trim().is_empty()) {
                    records.push((record_line, std::mem::take(&mut record)));
                } else {
                    record.clear();
                }
                any = false;
                line += 1;
                record_line = line;
            }
            _ => {
                field.push(c);
                any = true;
            }
        }
    }
    if in_quotes {
        return Err(bad(format!(
            "unterminated quoted field starting on line {record_line}"
        )));
    }
    if any || !field.is_empty() || !record.is_empty() {
        record.push(field);
        if record.iter().any(|f| !f.trim().is_empty()) {
            records.push((record_line, record));
        }
    }
    let mut it = records.into_iter();
    let headers = it
        .next()
        .map(|(_, h)| h.iter().map(|c| c.trim().to_owned()).collect())
        .unwrap_or_default();
    let rows = it
        .map(|(l, cells)| (l, cells.iter().map(|c| unguard(c)).collect()))
        .collect();
    Ok(Table { headers, rows })
}

fn cell_text(d: &calamine::Data) -> String {
    use calamine::Data;
    match d {
        Data::Empty => String::new(),
        Data::String(s) => unguard(s),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("{f:.0}")
            } else {
                let s = format!("{f:.6}");
                s.trim_end_matches('0').trim_end_matches('.').to_owned()
            }
        }
        Data::Bool(b) => b.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(t) if t.time() == chrono::NaiveTime::MIN => {
                t.date().format("%Y-%m-%d").to_string()
            }
            Some(t) => t.format("%Y-%m-%dT%H:%M:%S").to_string(),
            None => dt.as_f64().to_string(),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("#ERROR {e:?}"),
    }
}

/// First worksheet of an XLSX workbook. Line numbers are worksheet row numbers (1-based).
pub fn read_xlsx(bytes: &[u8]) -> Result<Table, KernelError> {
    use calamine::{open_workbook_from_rs, Reader, Xlsx};
    let mut wb: Xlsx<_> = open_workbook_from_rs(Cursor::new(bytes.to_vec()))
        .map_err(|e| bad(format!("not a readable XLSX workbook: {e}")))?;
    let range = wb
        .worksheet_range_at(0)
        .ok_or_else(|| bad("the workbook has no worksheet"))?
        .map_err(|e| bad(format!("the first worksheet is unreadable: {e}")))?;
    let (first_row, _) = range.start().unwrap_or((0, 0));
    let mut headers: Vec<String> = Vec::new();
    let mut have_headers = false;
    let mut rows = Vec::new();
    for (i, r) in range.rows().enumerate() {
        let cells: Vec<String> = r.iter().map(cell_text).collect();
        let line = first_row
            .saturating_add(u32::try_from(i).unwrap_or(u32::MAX - 1))
            .saturating_add(1);
        if !have_headers {
            headers = cells.iter().map(|c| c.trim().to_owned()).collect();
            have_headers = true;
            continue;
        }
        if cells.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        rows.push((line, cells));
    }
    // Trailing empty header cells (formatting beyond the data) are not columns.
    while headers.last().is_some_and(String::is_empty) {
        headers.pop();
    }
    Ok(Table { headers, rows })
}

/// Lower-case alphanumerics only: `Student No.` ≈ `student_no` ≈ `studentno`.
pub fn header_key(h: &str) -> String {
    h.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_reader_handles_quotes_crlf_bom_and_blank_lines() {
        let src = "\u{feff}name,note\r\n\"Doe, Jane\",\"said \"\"hi\"\"\"\r\n\r\nRaj,\"two\nlines\"\nAsha,\n";
        let t = read_csv(src.as_bytes()).unwrap();
        assert_eq!(t.headers, vec!["name", "note"]);
        assert_eq!(t.rows.len(), 3);
        assert_eq!(
            t.rows[0],
            (2, vec!["Doe, Jane".to_owned(), "said \"hi\"".to_owned()])
        );
        assert_eq!(t.rows[1].0, 4);
        assert_eq!(t.rows[1].1[1], "two\nlines");
        assert_eq!(t.rows[2], (6, vec!["Asha".to_owned(), String::new()]));
    }

    #[test]
    fn csv_reader_rejects_unterminated_quotes_and_bad_encoding() {
        assert!(read_csv(b"a,b\n\"x,y\n").is_err());
        assert!(read_csv(&[0x61, 0x0a, 0xff, 0xfe]).is_err());
    }

    #[test]
    fn exports_neutralise_formulas_and_imports_restore_them() {
        for evil in ["=HYPERLINK(\"http://x\")", "+1", "-2", "@SUM(A1)", "\tx"] {
            let c = csv_cell(evil);
            assert!(c.starts_with('\'') || c.starts_with("\"'"), "{evil} -> {c}");
        }
        for evil in ["=HYPERLINK(\"http://x\")", "+1", "-2", "@SUM(A1)"] {
            assert_eq!(unguard(&guard(evil)), evil);
        }
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        let bytes = write_csv(
            &["h".to_owned()],
            &[vec!["=1+1".to_owned()], vec!["x\"y".to_owned()]],
        );
        let back = read_csv(&bytes).unwrap();
        assert_eq!(back.rows[0].1[0], "=1+1");
        assert_eq!(back.rows[1].1[0], "x\"y");
    }

    #[test]
    fn xlsx_round_trip_keeps_text_and_guards_formulas() {
        let headers = vec!["code".to_owned(), "amount".to_owned(), "note".to_owned()];
        let rows = vec![
            vec![
                "A-1".to_owned(),
                "-1500.50".to_owned(),
                "=cmd|' /C calc'!A0".to_owned(),
            ],
            vec!["B-2".to_owned(), "20".to_owned(), String::new()],
        ];
        let bytes = write_xlsx("students", &headers, &rows).unwrap();
        let t = read_xlsx(&bytes).unwrap();
        assert_eq!(t.headers, headers);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].0, 2);
        assert_eq!(t.rows[0].1, rows[0]);
        assert_eq!(t.rows[1].1[0], "B-2");
    }

    #[test]
    fn read_validates_headers_and_size() {
        assert!(read(FileFormat::Csv, b"a,a\n1,2\n").is_err());
        assert!(read(FileFormat::Csv, b"a,b\n").is_err());
        assert!(read(FileFormat::Csv, b"a,b\n1,2\n").is_ok());
        assert_eq!(FileFormat::from_file_name("X.XLSX"), Some(FileFormat::Xlsx));
        assert_eq!(FileFormat::from_file_name("x.xls"), None);
        assert_eq!(header_key("Student No."), "studentno");
    }
}
