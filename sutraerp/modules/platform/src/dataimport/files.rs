//! Import / export file formats (Part 2 · S4-import): CSV (RFC 4180) and XLSX (Office Open XML
//! SpreadsheetML), both read and written here over the `zip` crate.
//!
//! Why not `calamine` / `rust_xlsxwriter`: every published `calamine` (≤ 0.36.1) and
//! `rust_xlsxwriter` (≤ 0.99.1) depends on `zip` 7.x / 8.x, and **every** `zip` 7.x / 8.x release
//! is yanked on crates.io (only `zip` ≥ 9.0.3 is current), so neither resolves without a yanked
//! crate — which cargo-deny refuses (`yanked = "deny"`, deny.toml). The SpreadsheetML subset an
//! import needs (first worksheet, shared / inline strings, numbers, booleans, dates by cell
//! style) and a one-sheet writer are implemented below on `zip` 9 (deflate via flate2/zlib-rs).
//!
//! **Formula injection** (SEC-P1-APPSEC-006, OWASP "CSV Injection"): every exported cell that
//! starts with `=`, `+`, `-`, `@`, TAB or CR is prefixed with a single quote, so a spreadsheet
//! never evaluates it — the same rule as the web's `csvCell` (`apps/web/app/lib/csv.ts`). XLSX
//! cells are always written as *inline strings* (never formulas) and get the same prefix. On import
//! a leading `'` in front of one of those characters is removed again, so an export re-imports
//! unchanged (round trip). Formulas in uploaded workbooks are never evaluated: only the cached
//! value is read.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use sutra_kernel::error::{FieldViolation, KernelError};

/// Upper bound of data rows in one file (bigger migrations are split into several files).
pub const MAX_ROWS: usize = 20_000;
/// Upper bound of columns in one file.
pub const MAX_COLUMNS: usize = 200;
/// Upper bound of one decompressed XLSX part (zip-bomb guard).
pub const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

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

// ------------------------------------------------------------------ minimal XML (SpreadsheetML)

/// One XML token. Names keep their prefix (`r:id`); [`local`] strips it.
#[derive(Debug, PartialEq, Eq)]
enum Tok<'a> {
    Open {
        name: &'a str,
        attrs: &'a str,
        empty: bool,
    },
    Close {
        name: &'a str,
    },
    /// Character data (still escaped).
    Text(&'a str),
    /// CDATA section content (literal).
    Cdata(&'a str),
}

fn local(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, l)| l)
}

/// Tokenises a well-formed XML document (declarations, comments, DOCTYPE skipped).
fn tokens(xml: &str) -> Result<Vec<Tok<'_>>, KernelError> {
    let mut out = Vec::new();
    let mut rest = xml;
    let broken = || bad("the workbook contains malformed XML");
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            out.push(Tok::Text(rest));
            break;
        };
        if lt > 0 {
            out.push(Tok::Text(&rest[..lt]));
        }
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<?") {
            rest = &r[r.find("?>").ok_or_else(broken)? + 2..];
        } else if let Some(r) = rest.strip_prefix("<!--") {
            rest = &r[r.find("-->").ok_or_else(broken)? + 3..];
        } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>").ok_or_else(broken)?;
            out.push(Tok::Cdata(&r[..end]));
            rest = &r[end + 3..];
        } else if let Some(r) = rest.strip_prefix("<!") {
            rest = &r[r.find('>').ok_or_else(broken)? + 1..];
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').ok_or_else(broken)?;
            out.push(Tok::Close {
                name: r[..end].trim(),
            });
            rest = &r[end + 1..];
        } else {
            let body = &rest[1..];
            let mut quote: Option<char> = None;
            let mut end = None;
            for (i, c) in body.char_indices() {
                match (quote, c) {
                    (None, '"' | '\'') => quote = Some(c),
                    (Some(q), _) if c == q => quote = None,
                    (None, '>') => {
                        end = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let end = end.ok_or_else(broken)?;
            let mut inner = &body[..end];
            let empty = inner.ends_with('/');
            if empty {
                inner = &inner[..inner.len() - 1];
            }
            let split = inner
                .find(|c: char| c.is_whitespace())
                .unwrap_or(inner.len());
            out.push(Tok::Open {
                name: &inner[..split],
                attrs: &inner[split..],
                empty,
            });
            rest = &body[end + 1..];
        }
    }
    Ok(out)
}

/// Value of attribute `name` (matched on the full or the local name), unescaped.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start();
        let eq = rest.find('=')?;
        let key = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let q = after.chars().next()?;
        if q != '"' && q != '\'' {
            return None;
        }
        let close = after[1..].find(q)?;
        let value = &after[1..=close];
        if key == name || local(key) == name {
            return Some(unescape(value));
        }
        rest = &after[close + 2..];
    }
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(semi) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let ent = &rest[1..semi];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=semi]),
        }
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            // Characters XML 1.0 cannot carry are dropped.
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// `A` → 0, `Z` → 25, `AA` → 26 (column letters of a cell reference like `AB12`).
fn column_index(cell_ref: &str) -> Option<usize> {
    let letters: String = cell_ref
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    if letters.is_empty() {
        return None;
    }
    let mut n = 0usize;
    for c in letters.chars() {
        n = n * 26 + usize::from(c.to_ascii_uppercase() as u8 - b'A') + 1;
    }
    Some(n - 1)
}

fn column_letters(mut i: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(char::from(b'A' + u8::try_from(i % 26).unwrap_or(0)));
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    s.iter().rev().collect()
}

// ------------------------------------------------------------------ XLSX write

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#;

const ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;

const WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><sz val="11"/><name val="Calibri"/></font><font><b/><sz val="11"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="2"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/></cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#;

fn sheet_xml(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut s = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<worksheet \
         xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    if !headers.is_empty() {
        s.push_str(&format!(
            "<cols><col min=\"1\" max=\"{}\" width=\"18\" customWidth=\"1\"/></cols>",
            headers.len()
        ));
    }
    s.push_str("<sheetData>");
    let all = std::iter::once(headers).chain(rows.iter().map(Vec::as_slice));
    for (r, cells) in all.enumerate() {
        let rn = r + 1;
        s.push_str(&format!("<row r=\"{rn}\">"));
        for (c, v) in cells.iter().enumerate() {
            if v.is_empty() {
                continue;
            }
            let style = if r == 0 { " s=\"1\"" } else { "" };
            s.push_str(&format!(
                "<c r=\"{}{rn}\" t=\"inlineStr\"{style}><is><t xml:space=\"preserve\">{}</t></is></c>",
                column_letters(c),
                escape(&guard(v))
            ));
        }
        s.push_str("</row>");
    }
    s.push_str("</sheetData></worksheet>");
    s
}

/// An XLSX workbook with one sheet: bold header row, every cell a guarded inline string.
pub fn write_xlsx(
    sheet: &str,
    headers: &[String],
    rows: &[Vec<String>],
) -> Result<Vec<u8>, KernelError> {
    use zip::write::SimpleFileOptions;
    let name: String = sheet
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(31)
        .collect();
    let name = if name.is_empty() {
        "data".to_owned()
    } else {
        name
    };
    let workbook = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<workbook \
         xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
         xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
         <sheets><sheet name=\"{}\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>",
        escape(&name)
    );
    let parts: [(&str, String); 6] = [
        ("[Content_Types].xml", CONTENT_TYPES.to_owned()),
        ("_rels/.rels", ROOT_RELS.to_owned()),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", WORKBOOK_RELS.to_owned()),
        ("xl/styles.xml", STYLES.to_owned()),
        ("xl/worksheets/sheet1.xml", sheet_xml(headers, rows)),
    ];
    let zip_err = |e: zip::result::ZipError| KernelError::internal_msg(format!("xlsx: {e}"));
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (path, content) in parts {
        w.start_file(path, opts).map_err(zip_err)?;
        w.write_all(content.as_bytes())
            .map_err(|e| KernelError::internal_msg(format!("xlsx: {e}")))?;
    }
    Ok(w.finish().map_err(zip_err)?.into_inner())
}

// ------------------------------------------------------------------ XLSX read

fn part<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
) -> Result<Option<String>, KernelError> {
    let mut f = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(bad(format!("unreadable workbook part {name}: {e}"))),
    };
    let mut buf = Vec::new();
    (&mut f)
        .take(MAX_PART_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|e| bad(format!("unreadable workbook part {name}: {e}")))?;
    if buf.len() as u64 > MAX_PART_BYTES {
        return Err(bad(format!("workbook part {name} is too large")));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| bad(format!("workbook part {name} is not UTF-8")))
}

/// Text of the `<t>` elements inside each `<si>` (phonetic `<rPh>` runs skipped).
fn shared_strings(xml: &str) -> Result<Vec<String>, KernelError> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let (mut in_t, mut in_rph) = (false, false);
    for t in tokens(xml)? {
        match t {
            Tok::Open { name, empty, .. } => match local(name) {
                "si" if empty => out.push(String::new()),
                "si" => cur = Some(String::new()),
                "rPh" if !empty => in_rph = true,
                "t" if !empty => in_t = true,
                _ => {}
            },
            Tok::Close { name } => match local(name) {
                "si" => out.push(cur.take().unwrap_or_default()),
                "rPh" => in_rph = false,
                "t" => in_t = false,
                _ => {}
            },
            Tok::Text(s) if in_t && !in_rph => {
                if let Some(c) = cur.as_mut() {
                    c.push_str(&unescape(s));
                }
            }
            Tok::Cdata(s) if in_t && !in_rph => {
                if let Some(c) = cur.as_mut() {
                    c.push_str(s);
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

fn is_date_format(id: u32, code: Option<&str>) -> bool {
    if (14..=22).contains(&id) || (45..=47).contains(&id) {
        return true;
    }
    let Some(code) = code else {
        return false;
    };
    // Strip quoted literals and [..] sections (colours, locales) before looking for d / m / y.
    let mut plain = String::new();
    let mut quoted = false;
    let mut bracket = false;
    for c in code.chars() {
        match c {
            '"' => quoted = !quoted,
            '[' if !quoted => bracket = true,
            ']' if !quoted => bracket = false,
            _ if !quoted && !bracket => plain.push(c.to_ascii_lowercase()),
            _ => {}
        }
    }
    plain.contains('y') || plain.contains('d') || (plain.contains('m') && !plain.contains('0'))
}

/// Indices of `cellXfs` whose number format is a date format.
fn date_styles(xml: &str) -> Result<Vec<bool>, KernelError> {
    let toks = tokens(xml)?;
    let mut custom: HashMap<u32, String> = HashMap::new();
    for t in &toks {
        if let Tok::Open { name, attrs, .. } = t {
            if local(name) == "numFmt" {
                if let (Some(id), Some(code)) = (attr(attrs, "numFmtId"), attr(attrs, "formatCode"))
                {
                    if let Ok(id) = id.parse() {
                        custom.insert(id, code);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut in_xfs = false;
    for t in &toks {
        match t {
            Tok::Open { name, .. } if local(name) == "cellXfs" => in_xfs = true,
            Tok::Close { name } if local(name) == "cellXfs" => in_xfs = false,
            Tok::Open { name, attrs, .. } if in_xfs && local(name) == "xf" => {
                let id: u32 = attr(attrs, "numFmtId")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                out.push(is_date_format(id, custom.get(&id).map(String::as_str)));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn serial_to_date(serial: f64, date1904: bool) -> Option<String> {
    let base = if date1904 {
        chrono::NaiveDate::from_ymd_opt(1904, 1, 1)?
    } else {
        chrono::NaiveDate::from_ymd_opt(1899, 12, 30)?
    };
    if !(0.0..=2_958_465.0).contains(&serial) {
        return None;
    }
    let days = serial.floor() as i64;
    let date = base.checked_add_signed(chrono::Duration::days(days))?;
    let secs = ((serial - serial.floor()) * 86_400.0).round() as u32;
    if secs == 0 {
        Some(date.format("%Y-%m-%d").to_string())
    } else {
        let t = chrono::NaiveTime::from_num_seconds_from_midnight_opt(secs.min(86_399), 0)?;
        Some(date.and_time(t).format("%Y-%m-%dT%H:%M:%S").to_string())
    }
}

fn number_text(v: &str) -> String {
    match v.trim().parse::<f64>() {
        Ok(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{f:.0}"),
        Ok(f) => {
            let s = format!("{f:.10}");
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        }
        Err(_) => v.trim().to_owned(),
    }
}

/// First worksheet of an XLSX workbook. Line numbers are worksheet row numbers (1-based).
pub fn read_xlsx(bytes: &[u8]) -> Result<Table, KernelError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| bad(format!("not a readable XLSX workbook: {e}")))?;
    let workbook = part(&mut zip, "xl/workbook.xml")?.ok_or_else(|| bad("not an XLSX workbook"))?;
    let wtoks = tokens(&workbook)?;
    let mut sheet_rid = None;
    let mut date1904 = false;
    for t in &wtoks {
        if let Tok::Open { name, attrs, .. } = t {
            match local(name) {
                "sheet" if sheet_rid.is_none() => sheet_rid = attr(attrs, "r:id"),
                "workbookPr" => {
                    date1904 = matches!(attr(attrs, "date1904").as_deref(), Some("1" | "true"));
                }
                _ => {}
            }
        }
    }
    let mut sheet_path = "xl/worksheets/sheet1.xml".to_owned();
    if let (Some(rid), Some(rels)) = (sheet_rid, part(&mut zip, "xl/_rels/workbook.xml.rels")?) {
        for t in tokens(&rels)? {
            if let Tok::Open { name, attrs, .. } = t {
                if local(name) == "Relationship" && attr(attrs, "Id").as_deref() == Some(&rid) {
                    if let Some(target) = attr(attrs, "Target") {
                        sheet_path = match target.strip_prefix('/') {
                            Some(abs) => abs.to_owned(),
                            None => format!("xl/{target}"),
                        };
                    }
                }
            }
        }
    }
    let shared = match part(&mut zip, "xl/sharedStrings.xml")? {
        Some(x) => shared_strings(&x)?,
        None => vec![],
    };
    let dates = match part(&mut zip, "xl/styles.xml")? {
        Some(x) => date_styles(&x)?,
        None => vec![],
    };
    let sheet = part(&mut zip, &sheet_path)?.ok_or_else(|| bad("the workbook has no worksheet"))?;

    let mut rows: Vec<(u32, Vec<String>)> = Vec::new();
    let mut row_no: u32 = 0;
    let mut cells: Vec<String> = Vec::new();
    let mut next_col = 0usize;
    // Current cell: (column, type, style), its <v> / inline text.
    let mut cell: Option<(usize, String, usize)> = None;
    let mut value = String::new();
    let mut in_v = false;
    let mut in_is_t = false;
    let mut in_row = false;
    let finish_cell = |cells: &mut Vec<String>, col: usize, ty: &str, style: usize, v: &str| {
        let text = match ty {
            "s" => v
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|i| shared.get(i).cloned())
                .unwrap_or_default(),
            "inlineStr" | "str" => v.to_owned(),
            "b" => if v.trim() == "1" { "true" } else { "false" }.to_owned(),
            "e" => format!("#ERROR {}", v.trim()),
            "d" => v.trim().to_owned(),
            _ => {
                if dates.get(style).copied().unwrap_or(false) {
                    v.trim()
                        .parse::<f64>()
                        .ok()
                        .and_then(|f| serial_to_date(f, date1904))
                        .unwrap_or_else(|| number_text(v))
                } else {
                    number_text(v)
                }
            }
        };
        if col < MAX_COLUMNS * 4 {
            if cells.len() <= col {
                cells.resize(col + 1, String::new());
            }
            cells[col] = unguard(&text);
        }
    };
    for t in tokens(&sheet)? {
        match t {
            Tok::Open { name, attrs, empty } => match local(name) {
                "row" => {
                    row_no = attr(attrs, "r")
                        .and_then(|r| r.parse().ok())
                        .unwrap_or(row_no + 1);
                    cells = Vec::new();
                    next_col = 0;
                    in_row = !empty;
                    if empty {
                        rows.push((row_no, Vec::new()));
                    }
                }
                "c" if in_row => {
                    let col = attr(attrs, "r")
                        .and_then(|r| column_index(&r))
                        .unwrap_or(next_col);
                    next_col = col + 1;
                    let ty = attr(attrs, "t").unwrap_or_else(|| "n".to_owned());
                    let style = attr(attrs, "s").and_then(|s| s.parse().ok()).unwrap_or(0);
                    value.clear();
                    if !empty {
                        cell = Some((col, ty, style));
                    }
                }
                "v" if cell.is_some() && !empty => in_v = true,
                "t" if cell.is_some() && !empty => in_is_t = true,
                _ => {}
            },
            Tok::Close { name } => match local(name) {
                "v" => in_v = false,
                "t" => in_is_t = false,
                "c" => {
                    if let Some((col, ty, style)) = cell.take() {
                        finish_cell(&mut cells, col, &ty, style, &value);
                    }
                }
                "row" => {
                    in_row = false;
                    rows.push((row_no, std::mem::take(&mut cells)));
                }
                _ => {}
            },
            Tok::Text(s) if in_v || in_is_t => value.push_str(&unescape(s)),
            Tok::Cdata(s) if in_v || in_is_t => value.push_str(s),
            _ => {}
        }
    }
    let mut it = rows.into_iter();
    let mut headers: Vec<String> = it
        .next()
        .map(|(_, h)| h.iter().map(|c| c.trim().to_owned()).collect())
        .unwrap_or_default();
    while headers.last().is_some_and(String::is_empty) {
        headers.pop();
    }
    let data = it
        .filter(|(_, cells)| cells.iter().any(|c| !c.trim().is_empty()))
        .collect();
    Ok(Table {
        headers,
        rows: data,
    })
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
                "=cmd|' /C calc'!A0 & <b>".to_owned(),
            ],
            vec!["B-2".to_owned(), "20".to_owned(), String::new()],
        ];
        let bytes = write_xlsx("students", &headers, &rows).unwrap();
        // The stored cell is a guarded inline string, never a formula.
        let mut z = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();
        let sheet = part(&mut z, "xl/worksheets/sheet1.xml").unwrap().unwrap();
        assert!(!sheet.contains("<f>"));
        assert!(sheet.contains("&apos;=cmd") || sheet.contains("'=cmd"));
        let t = read_xlsx(&bytes).unwrap();
        assert_eq!(t.headers, headers);
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].0, 2);
        assert_eq!(t.rows[0].1, rows[0]);
        assert_eq!(t.rows[1].1[0], "B-2");
        assert_eq!(t.rows[1].1[1], "20");
    }

    #[test]
    fn xlsx_reader_resolves_shared_strings_numbers_and_dates() {
        // A workbook as Excel writes it: shared strings, a number, a date-styled serial, a bool.
        use zip::write::SimpleFileOptions;
        let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let o = SimpleFileOptions::default();
        let parts = [
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0"?><workbook xmlns:r="x"><workbookPr/><sheets><sheet name="S" sheetId="1" r:id="rId7"/></sheets></workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<Relationships><Relationship Id="rId7" Target="worksheets/data.xml"/></Relationships>"#,
            ),
            (
                "xl/sharedStrings.xml",
                r#"<sst><si><t>name</t></si><si><t>dob</t></si><si><r><t>Asha </t></r><r><t xml:space="preserve">K &amp; Co</t></r><rPh><t>x</t></rPh></si><si><t>n</t></si><si><t>ok</t></si></sst>"#,
            ),
            (
                "xl/styles.xml",
                r#"<styleSheet><numFmts count="1"><numFmt numFmtId="164" formatCode="dd/mm/yyyy"/></numFmts><cellXfs count="3"><xf numFmtId="0"/><xf numFmtId="14"/><xf numFmtId="164"/></cellXfs></styleSheet>"#,
            ),
            (
                "xl/worksheets/data.xml",
                r#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c><c r="C1" t="s"><v>3</v></c><c r="D1" t="s"><v>4</v></c></row><row r="3"><c r="A3" t="s"><v>2</v></c><c r="B3" s="2"><v>46188</v></c><c r="C3"><v>12.5</v></c><c r="D3" t="b"><v>1</v></c></row></sheetData></worksheet>"#,
            ),
        ];
        for (p, c) in parts {
            w.start_file(p, o).unwrap();
            w.write_all(c.as_bytes()).unwrap();
        }
        let bytes = w.finish().unwrap().into_inner();
        let t = read_xlsx(&bytes).unwrap();
        assert_eq!(t.headers, vec!["name", "dob", "n", "ok"]);
        assert_eq!(t.rows.len(), 1);
        assert_eq!(t.rows[0].0, 3);
        assert_eq!(
            t.rows[0].1,
            vec!["Asha K & Co", "2026-06-15", "12.5", "true"]
        );
    }

    #[test]
    fn xml_helpers() {
        assert_eq!(column_index("A1"), Some(0));
        assert_eq!(column_index("Z9"), Some(25));
        assert_eq!(column_index("AA10"), Some(26));
        assert_eq!(column_letters(0), "A");
        assert_eq!(column_letters(26), "AA");
        assert_eq!(column_letters(27), "AB");
        assert_eq!(unescape("a&lt;b&#65;&#x42;&amp;"), "a<bAB&");
        assert_eq!(
            attr(r#" r:id="rId1" name='x &amp; y'"#, "id").as_deref(),
            Some("rId1")
        );
        assert_eq!(
            attr(r#" r:id="rId1" name='x &amp; y'"#, "name").as_deref(),
            Some("x & y")
        );
        assert!(is_date_format(14, None));
        assert!(is_date_format(164, Some("yyyy-mm-dd")));
        assert!(!is_date_format(164, Some("#,##0.00")));
        assert!(!is_date_format(164, Some("[Red]0.00")));
        assert_eq!(
            serial_to_date(46188.0, false).as_deref(),
            Some("2026-06-15")
        );
    }

    #[test]
    fn read_validates_headers_and_size() {
        assert!(read(FileFormat::Csv, b"a,a\n1,2\n").is_err());
        assert!(read(FileFormat::Csv, b"a,b\n").is_err());
        assert!(read(FileFormat::Csv, b"a,b\n1,2\n").is_ok());
        assert!(read(FileFormat::Xlsx, b"not a zip").is_err());
        assert_eq!(FileFormat::from_file_name("X.XLSX"), Some(FileFormat::Xlsx));
        assert_eq!(FileFormat::from_file_name("x.xls"), None);
        assert_eq!(header_key("Student No."), "studentno");
    }
}
