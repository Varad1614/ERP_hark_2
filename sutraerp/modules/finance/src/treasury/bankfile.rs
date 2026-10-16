//! Bank statement files: CSV (any bank's export, with a column mapping), OFX/QFX and SWIFT MT940,
//! normalised to signed paise lines, plus line-level de-duplication against earlier imports.
//!
//! The CSV, date, amount, column-guessing, OFX and de-duplication rules are ported from
//! Everything's `accounting/src/lib/bankimport.js`, `csv.js` and `money.js` `parseMoney`
//! (MIT, HeyPuter/everything @ 06802a1) and are checked against the originals with golden vectors
//! (`tests/golden/bankimport_*.json`). The Indian layer on top ([`guess_mapping_in`], header
//! detection below a preamble, `Dr/Cr` indicator and reference columns) and MT940 are SutraERP's
//! own. Everything is pure: no I/O, no floats.

use chrono::NaiveDate;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// One normalised statement line. `amount` is in paise: positive = money into the account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub date: NaiveDate,
    pub amount: i64,
    pub desc: String,
    /// The bank's own transaction id (OFX `FITID`, MT940 bank reference); empty when the file has none.
    pub fitid: String,
    /// Cheque / UTR / customer reference, when the file carries one.
    pub reference: String,
}

/// A data row that produced no line, with the reason (1-based row number in the file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub row: usize,
    pub reason: String,
}

/// Day-month order for ambiguous numeric dates such as `03/04/2026`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateStyle {
    Mdy,
    Dmy,
}

/// Column positions in a CSV statement (`-1` = absent), as in the original.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mapping {
    pub date: i32,
    pub desc: i32,
    pub amount: i32,
    pub debit: i32,
    pub credit: i32,
    /// `true` = separate debit / credit columns, `false` = one signed amount column.
    pub split: bool,
    /// SutraERP: a `Dr`/`Cr` indicator column that signs an unsigned amount column.
    pub indicator: i32,
    /// SutraERP: a cheque / UTR / reference column.
    pub reference: i32,
}

impl Mapping {
    fn single(date: i32, desc: i32, amount: i32) -> Self {
        Mapping {
            date,
            desc,
            amount,
            debit: -1,
            credit: -1,
            split: false,
            indicator: -1,
            reference: -1,
        }
    }
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap_or_else(|e| panic!("static regex {p}: {e}"))
}

// ------------------------------------------------------------------------------------- CSV

/// RFC 4180 CSV (quotes, escaped quotes, CRLF, embedded newlines); the delimiter is guessed from
/// the first line when not given. Port of `csv.js` `parseCSV`.
pub fn parse_csv(text: &str, delimiter: Option<char>) -> Vec<Vec<String>> {
    let src = text.strip_prefix('\u{feff}').unwrap_or(text);
    let delim = delimiter.unwrap_or_else(|| detect_delimiter(src));
    let chars: Vec<char> = src.chars().collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let push_row = |rows: &mut Vec<Vec<String>>, row: Vec<String>| {
        if row.len() > 1 || row.first().is_some_and(|c| !c.is_empty()) {
            rows.push(row);
        }
    };
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if quoted {
            if ch == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    field.push('"');
                    i += 1;
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            i += 1;
            continue;
        }
        if ch == '"' && js_trim(&field).is_empty() {
            field.clear();
            quoted = true;
        } else if ch == delim {
            row.push(std::mem::take(&mut field));
        } else if ch == '\n' || ch == '\r' {
            if ch == '\r' && chars.get(i + 1) == Some(&'\n') {
                i += 1;
            }
            row.push(std::mem::take(&mut field));
            push_row(&mut rows, std::mem::take(&mut row));
        } else {
            field.push(ch);
        }
        i += 1;
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        push_row(&mut rows, row);
    }
    rows.into_iter()
        .map(|r| r.into_iter().map(|c| js_trim(&c).to_string()).collect())
        .collect()
}

fn detect_delimiter(src: &str) -> char {
    let first = src.split('\n').next().unwrap_or("");
    let first = first.strip_suffix('\r').unwrap_or(first);
    let mut best = (',', 0usize);
    for d in [',', ';', '\t', '|'] {
        let n = first.matches(d).count();
        if n > best.1 {
            best = (d, n);
        }
    }
    best.0
}

/// JavaScript's `String.prototype.trim` whitespace set.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

// ----------------------------------------------------------------------------------- money

/// Money text from a bank export to paise: `1,00,000.50`, `(12.00)`, `-₹ 5`, `12,5` (decimal
/// comma), `1.234,56`. Unreadable text is 0. Port of `money.js` `parseMoney`.
pub fn parse_money(text: &str) -> i64 {
    let mut s: String = js_trim(text)
        .chars()
        .map(|c| match c {
            '\u{2212}' | '\u{2012}' | '\u{2013}' | '\u{fe63}' | '\u{ff0d}' => '-',
            c => c,
        })
        .collect();
    if s.is_empty() {
        return 0;
    }
    let mut negative = false;
    if s.len() >= 2 && s.starts_with('(') && s.ends_with(')') && !s.contains('\n') {
        negative = true;
        s = s[1..s.len() - 1].to_string();
    }
    if s.contains('-') {
        negative = !negative;
    }
    let s: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    if s.is_empty() {
        return 0;
    }
    let last_dot = s.rfind('.');
    let last_comma = s.rfind(',');
    let decimal_sep = match (last_dot, last_comma) {
        (Some(d), Some(c)) => Some(if d > c { '.' } else { ',' }),
        (None, Some(c)) => {
            let tail = s.len() - c - 1;
            if tail == 3 && s.find(',') == Some(c) {
                None
            } else if tail <= 2 {
                Some(',')
            } else {
                None
            }
        }
        (Some(d), None) => {
            let tail = s.len() - d - 1;
            let dots = s.matches('.').count();
            if dots > 1 && tail == 3 {
                None
            } else {
                Some('.')
            }
        }
        (None, None) => None,
    };
    let (int_part, frac_part) = match decimal_sep.and_then(|sep| s.rfind(sep)) {
        Some(at) => (&s[..at], &s[at + 1..]),
        None => (s.as_str(), ""),
    };
    let int_digits: String = int_part.chars().filter(char::is_ascii_digit).collect();
    let frac: String = frac_part
        .chars()
        .filter(char::is_ascii_digit)
        .chain("00".chars())
        .collect();
    let int_value: i128 = if int_digits.is_empty() {
        0
    } else {
        match int_digits.parse::<i128>() {
            Ok(v) => v,
            Err(_) => return 0,
        }
    };
    let digit = |i: usize| -> i128 {
        frac.as_bytes()
            .get(i)
            .map_or(0, |b| i128::from(b.saturating_sub(b'0')))
    };
    let mut paise = int_value
        .saturating_mul(100)
        .saturating_add(digit(0) * 10 + digit(1));
    if digit(2) >= 5 {
        paise += 1;
    }
    let Ok(v) = i64::try_from(paise) else {
        return 0;
    };
    if negative {
        -v
    } else {
        v
    }
}

// ----------------------------------------------------------------------------------- dates

static D_NUMERIC: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([0-9]{1,2})[/.\-]([0-9]{1,2})[/.\-]([0-9]{2,4})"));
static D_YMD: LazyLock<Regex> = LazyLock::new(|| re(r"^([0-9]{4})[/.]([0-9]{1,2})[/.]([0-9]{1,2})"));
static D_MON_FIRST: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([A-Za-z]{3})[a-z]*\.?\s+([0-9]{1,2}),?\s+([0-9]{4})"));
static D_DAY_FIRST: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([0-9]{1,2})[\s-]([A-Za-z]{3})[a-z]*[\s-]([0-9]{2,4})"));

fn month_of(name: &str) -> Option<u32> {
    let n = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ]
    .iter()
    .position(|m| *m == name.to_ascii_lowercase())?;
    u32::try_from(n + 1).ok()
}

/// `isISODate` on `YYYY-MM-DD` text.
fn iso(text: &str) -> Option<NaiveDate> {
    let b = text.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

fn ymd(y: &str, m: u32, d: u32) -> Option<NaiveDate> {
    iso(&format!("{y}-{m:02}-{d:02}"))
}

/// A statement date: ISO, `YYYYMMDD`, `03/04/2026` (order from `style`, or day-first when the
/// first number exceeds 12), `2026/3/4`, `Mar 4, 2026`, `4 Mar 2026`, `04-Mar-26`.
/// Port of `bankimport.js` `parseStatementDate`.
pub fn parse_statement_date(text: &str, style: DateStyle) -> Option<NaiveDate> {
    let s = js_trim(text);
    if s.is_empty() {
        return None;
    }
    let head: String = s.chars().take(10).collect();
    if let Some(d) = iso(&head) {
        return Some(d);
    }
    if s.len() >= 8 && s.as_bytes()[..8].iter().all(u8::is_ascii_digit) {
        return iso(&format!("{}-{}-{}", &s[0..4], &s[4..6], &s[6..8]));
    }
    let num = |t: &str| t.parse::<u32>().unwrap_or(0);
    if let Some(c) = D_NUMERIC.captures(s) {
        let (a, b, y) = (&c[1], &c[2], &c[3]);
        let y = if y.len() == 2 {
            format!("20{y}")
        } else {
            y.to_string()
        };
        let (mut month, mut day) = (num(a), num(b));
        if style == DateStyle::Dmy || month > 12 {
            month = num(b);
            day = num(a);
        }
        return ymd(&y, month, day);
    }
    if let Some(c) = D_YMD.captures(s) {
        return ymd(&c[1], num(&c[2]), num(&c[3]));
    }
    if let Some(c) = D_MON_FIRST.captures(s) {
        if let Some(m) = month_of(&c[1]) {
            return ymd(&c[3], m, num(&c[2]));
        }
    }
    if let Some(c) = D_DAY_FIRST.captures(s) {
        if let Some(m) = month_of(&c[2]) {
            let y = if c[3].len() == 2 {
                format!("20{}", &c[3])
            } else {
                c[3].to_string()
            };
            return ymd(&y, m, num(&c[1]));
        }
    }
    None
}

// ------------------------------------------------------------------------- column mapping

const DATE_HEADERS: &[&str] = &[
    "date",
    "transaction date",
    "posted date",
    "posting date",
    "trans date",
    "value date",
    "booking date",
];
const DESC_HEADERS: &[&str] = &[
    "description",
    "payee",
    "name",
    "memo",
    "details",
    "transaction description",
    "narrative",
    "merchant",
];
const AMOUNT_HEADERS: &[&str] = &["amount", "transaction amount", "value", "amount (usd)"];
const DEBIT_HEADERS: &[&str] = &[
    "debit",
    "withdrawal",
    "withdrawals",
    "money out",
    "paid out",
    "spent",
    "debit amount",
    "charges",
];
const CREDIT_HEADERS: &[&str] = &[
    "credit",
    "deposit",
    "deposits",
    "money in",
    "paid in",
    "received",
    "credit amount",
    "payments",
];

fn find(headers: &[String], names: &[&str]) -> i32 {
    let lower: Vec<String> = headers.iter().map(|h| js_trim(h).to_lowercase()).collect();
    let idx = |i: usize| i32::try_from(i).unwrap_or(-1);
    for n in names {
        if let Some(i) = lower.iter().position(|h| h == n) {
            return idx(i);
        }
    }
    for n in names {
        if let Some(i) = lower.iter().position(|h| h.contains(n)) {
            return idx(i);
        }
    }
    -1
}

/// Guesses the column mapping from a header row. Port of `bankimport.js` `guessMapping`.
pub fn guess_mapping(headers: &[String]) -> Mapping {
    let date = find(headers, DATE_HEADERS);
    let desc = find(headers, DESC_HEADERS);
    let amount = find(headers, AMOUNT_HEADERS);
    let mut debit = find(headers, DEBIT_HEADERS);
    let mut credit = find(headers, CREDIT_HEADERS);
    if debit >= 0 && debit == credit {
        debit = -1;
        credit = -1;
    }
    if amount >= 0 && !(debit >= 0 && credit >= 0) {
        return Mapping::single(date, desc, amount);
    }
    if debit >= 0 || credit >= 0 {
        return Mapping {
            date,
            desc,
            amount: -1,
            debit,
            credit,
            split: true,
            indicator: -1,
            reference: -1,
        };
    }
    Mapping::single(date, desc, amount)
}

const IN_DATE: &[&str] = &["txn date", "tran date", "transaction date", "date", "value dt", "value date"];
const IN_DESC: &[&str] = &[
    "narration",
    "particulars",
    "transaction remarks",
    "remarks",
    "description",
    "details",
];
const IN_DEBIT: &[&str] = &[
    "withdrawal amt",
    "withdrawal amount",
    "withdrawal",
    "debit amount",
    "debit",
    "dr amount",
];
const IN_CREDIT: &[&str] = &[
    "deposit amt",
    "deposit amount",
    "deposit",
    "credit amount",
    "credit",
    "cr amount",
];
const IN_INDICATOR: &[&str] = &[
    "dr/cr",
    "cr/dr",
    "dr / cr",
    "cr / dr",
    "debit/credit",
    "credit/debit",
    "dr or cr",
];
const IN_REFERENCE: &[&str] = &[
    "chq./ref.no.",
    "chq/ref no",
    "chq / ref no",
    "ref no./cheque no.",
    "cheque no",
    "chq no",
    "utr",
    "reference",
    "ref no",
];

fn find_exact_or_contains(headers: &[String], names: &[&str], not: &[i32]) -> i32 {
    let lower: Vec<String> = headers.iter().map(|h| js_trim(h).to_lowercase()).collect();
    let ok = |i: usize| {
        let i = i32::try_from(i).unwrap_or(-1);
        (!not.contains(&i)).then_some(i)
    };
    for n in names {
        if let Some(i) = lower.iter().position(|h| h == n).and_then(ok) {
            return i;
        }
    }
    for n in names {
        if let Some(i) = lower
            .iter()
            .enumerate()
            .filter(|(_, h)| h.contains(n))
            .find_map(|(i, _)| ok(i))
        {
            return i;
        }
    }
    -1
}

/// Indian bank exports (SBI, HDFC, ICICI, Axis, Kotak …): `Narration` / `Particulars`,
/// `Withdrawal Amt.` / `Deposit Amt.`, an unsigned amount with a `Dr/Cr` column, and a
/// `Chq./Ref.No.` or UTR column. Falls back to [`guess_mapping`] for anything it does not know.
pub fn guess_mapping_in(headers: &[String]) -> Mapping {
    let mut m = guess_mapping(headers);
    let indicator = find_exact_or_contains(headers, IN_INDICATOR, &[]);
    let reference = find_exact_or_contains(headers, IN_REFERENCE, &[indicator]);
    let date = find_exact_or_contains(headers, IN_DATE, &[]);
    if date >= 0 {
        m.date = date;
    }
    let desc = find_exact_or_contains(headers, IN_DESC, &[reference]);
    if desc >= 0 {
        m.desc = desc;
    }
    let taken = [m.date, m.desc, indicator, reference];
    let debit = find_exact_or_contains(headers, IN_DEBIT, &taken);
    let credit = find_exact_or_contains(headers, IN_CREDIT, &taken);
    if debit >= 0 && credit >= 0 && debit != credit {
        m.debit = debit;
        m.credit = credit;
        m.amount = -1;
        m.split = true;
    }
    if indicator >= 0 {
        if m.debit == indicator || m.credit == indicator {
            m.debit = -1;
            m.credit = -1;
            m.split = false;
        }
        if !m.split {
            if m.amount < 0 || m.amount == indicator {
                m.amount = find_exact_or_contains(headers, &["amount", "transaction amount"], &[indicator]);
            }
            m.indicator = indicator;
        }
    }
    m.reference = reference;
    m
}

// ------------------------------------------------------------------------------ CSV lines

/// Options for turning CSV rows into lines.
#[derive(Clone, Copy, Debug)]
pub struct CsvOptions {
    pub date_style: DateStyle,
    /// Flip every sign (card issuers that export charges as positive numbers).
    pub invert: bool,
}

fn cell(r: &[String], i: i32) -> &str {
    usize::try_from(i)
        .ok()
        .and_then(|i| r.get(i))
        .map_or("", String::as_str)
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// CSV rows to lines with a mapping. Port of `bankimport.js` `linesFromCSV`, plus the
/// indicator and reference columns of [`guess_mapping_in`].
pub fn lines_from_csv(
    rows: &[Vec<String>],
    mapping: &Mapping,
    header_rows: usize,
    opts: CsvOptions,
) -> (Vec<Line>, Vec<Skipped>) {
    let mut lines = Vec::new();
    let mut skipped = Vec::new();
    for (i, r) in rows.iter().enumerate().skip(header_rows) {
        let row = i + 1;
        let Some(date) = parse_statement_date(cell(r, mapping.date), opts.date_style) else {
            skipped.push(Skipped {
                row,
                reason: "No readable date".into(),
            });
            continue;
        };
        let mut amount = if mapping.split {
            let out = if mapping.debit >= 0 {
                parse_money(cell(r, mapping.debit)).abs()
            } else {
                0
            };
            let inn = if mapping.credit >= 0 {
                parse_money(cell(r, mapping.credit)).abs()
            } else {
                0
            };
            inn - out
        } else {
            parse_money(cell(r, mapping.amount))
        };
        if mapping.indicator >= 0 {
            let ind = cell(r, mapping.indicator).trim().to_ascii_lowercase();
            if ind.starts_with('d') || ind.starts_with("withdraw") {
                amount = -amount.abs();
            } else if ind.starts_with('c') || ind.starts_with("deposit") {
                amount = amount.abs();
            }
        }
        if opts.invert {
            amount = -amount;
        }
        if amount == 0 {
            skipped.push(Skipped {
                row,
                reason: "No amount".into(),
            });
            continue;
        }
        let desc = if mapping.desc >= 0 {
            cell(r, mapping.desc)
        } else {
            ""
        };
        lines.push(Line {
            date,
            amount,
            desc: squash(desc),
            fitid: String::new(),
            reference: squash(cell(r, mapping.reference)),
        });
    }
    (lines, skipped)
}

/// A parsed CSV statement.
#[derive(Clone, Debug)]
pub struct CsvStatement {
    pub headers: Vec<String>,
    /// Rows above and including the header (0 = no header row).
    pub header_rows: usize,
    pub mapping: Mapping,
    pub lines: Vec<Line>,
    pub skipped: Vec<Skipped>,
}

fn looks_like_header(row: &[String]) -> bool {
    row.iter().any(|h| h.chars().any(|c| c.is_ascii_alphabetic()))
        && parse_statement_date(row.first().map_or("", String::as_str), DateStyle::Mdy).is_none()
}

/// Exact port of `bankimport.js` `parseCSVStatement` (header on the first row, original mapping).
pub fn parse_csv_statement_original(text: &str, opts: CsvOptions) -> Option<CsvStatement> {
    let rows = parse_csv(text, None);
    let first = rows.first()?;
    let has_header = looks_like_header(first);
    let mapping = if has_header {
        guess_mapping(first)
    } else {
        Mapping::single(0, 1, 2)
    };
    let header_rows = usize::from(has_header);
    let (lines, skipped) = lines_from_csv(&rows, &mapping, header_rows, opts);
    Some(CsvStatement {
        headers: column_names(first, has_header),
        header_rows,
        mapping,
        lines,
        skipped,
    })
}

fn column_names(row: &[String], has_header: bool) -> Vec<String> {
    if has_header {
        row.to_vec()
    } else {
        (1..=row.len()).map(|i| format!("Column {i}")).collect()
    }
}

/// An Indian bank CSV: the header may sit below a preamble (account holder, branch, period —
/// SBI and ICICI do this), and the mapping understands Indian column names. A caller-supplied
/// mapping wins.
pub fn parse_csv_statement(
    text: &str,
    mapping: Option<Mapping>,
    opts: CsvOptions,
) -> Option<CsvStatement> {
    let rows = parse_csv(text, None);
    rows.first()?;
    let header_at = rows.iter().take(40).position(|r| {
        if !looks_like_header(r) {
            return false;
        }
        let m = guess_mapping_in(r);
        m.date >= 0 && (m.amount >= 0 || m.debit >= 0 || m.credit >= 0)
    });
    let (header_rows, headers, guessed) = match header_at {
        Some(h) => (h + 1, rows[h].clone(), guess_mapping_in(&rows[h])),
        None => {
            let first = &rows[0];
            let has = looks_like_header(first);
            let guessed = if has {
                guess_mapping_in(first)
            } else {
                Mapping::single(0, 1, 2)
            };
            (usize::from(has), column_names(first, has), guessed)
        }
    };
    let mapping = mapping.unwrap_or(guessed);
    let (lines, mut skipped) = lines_from_csv(&rows, &mapping, header_rows, opts);
    skipped.retain(|s| s.row > header_rows);
    Some(CsvStatement {
        headers,
        header_rows,
        mapping,
        lines,
        skipped,
    })
}

// ------------------------------------------------------------------------------------- OFX

/// One account's statement inside an OFX file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfxStatement {
    pub lines: Vec<Line>,
    /// Ledger balance in paise, when the file states it.
    pub balance: Option<i64>,
    pub account_number: String,
    pub account_type: String,
}

static OFX_STMT: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<(STMTRS|CCSTMTRS)>"));
static OFX_TRN: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)<STMTTRN>"));
static OFX_TRN_END: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</STMTTRN>"));
static OFX_BAL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)<LEDGERBAL>[\s\S]*?<BALAMT>([^<\r\n]*)"));

/// OFX 1.x (SGML) and 2.x (XML), one entry per account statement in the file.
/// Port of `bankimport.js` `parseOFX`.
pub fn parse_ofx(text: &str) -> Vec<OfxStatement> {
    let marks: Vec<_> = OFX_STMT.captures_iter(text).collect();
    if marks.is_empty() {
        return vec![ofx_statement(text, false)];
    }
    let mut out = Vec::new();
    for (k, c) in marks.iter().enumerate() {
        let (Some(all), Some(tag)) = (c.get(0), c.get(1)) else {
            continue;
        };
        let end = marks
            .get(k + 1)
            .and_then(|n| n.get(0))
            .map_or(text.len(), |m| m.start());
        let card = tag.as_str().to_ascii_lowercase().starts_with("cc");
        out.push(ofx_statement(&text[all.end()..end], card));
    }
    out
}

fn ofx_tag(block: &str, name: &str) -> String {
    Regex::new(&format!(r"(?i)<{name}>([^<\r\n]*)"))
        .ok()
        .and_then(|r| r.captures(block))
        .and_then(|c| c.get(1))
        .map(|m| js_trim(m.as_str()).to_string())
        .unwrap_or_default()
}

fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
}

fn ofx_statement(src: &str, card: bool) -> OfxStatement {
    let mut lines = Vec::new();
    for raw in OFX_TRN.split(src).skip(1) {
        let block = OFX_TRN_END.split(raw).next().unwrap_or("");
        let posted: String = ofx_tag(block, "DTPOSTED").chars().take(8).collect();
        let date = parse_statement_date(&posted, DateStyle::Mdy);
        let amount = parse_money(&ofx_tag(block, "TRNAMT"));
        let Some(date) = date else { continue };
        if amount == 0 {
            continue;
        }
        let name = decode_entities(&ofx_tag(block, "NAME"));
        let memo = decode_entities(&ofx_tag(block, "MEMO"));
        let mut parts = Vec::new();
        if !name.is_empty() {
            parts.push(name.clone());
        }
        if !memo.is_empty() && memo != name {
            parts.push(memo);
        }
        lines.push(Line {
            date,
            amount,
            desc: squash(&parts.join(" ")),
            fitid: ofx_tag(block, "FITID"),
            reference: ofx_tag(block, "CHECKNUM"),
        });
    }
    let balance = OFX_BAL
        .captures(src)
        .and_then(|c| c.get(1))
        .map(|m| parse_money(m.as_str()));
    let account_number = Regex::new(r"(?i)<ACCTID>([^<\r\n]*)")
        .ok()
        .and_then(|r| r.captures(src))
        .and_then(|c| c.get(1))
        .map(|m| js_trim(m.as_str()).to_string())
        .unwrap_or_default();
    let account_type = if card {
        "CREDITCARD".to_string()
    } else {
        ofx_tag(src, "ACCTTYPE").to_uppercase()
    };
    OfxStatement {
        lines,
        balance,
        account_number,
        account_type,
    }
}

/// `true` when the text is an OFX/QFX/QBO file.
pub fn is_ofx(text: &str) -> bool {
    let head: String = text.chars().take(2000).collect::<String>().to_uppercase();
    head.contains("<OFX>") || head.contains("OFXHEADER")
}

// ----------------------------------------------------------------------------------- MT940

/// A SWIFT MT940 customer statement (the format Indian banks give corporate accounts over
/// host-to-host / SFTP).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mt940 {
    pub reference: String,
    pub account: String,
    pub statement_no: String,
    /// Opening / closing booked balances in paise (`:60F:` / `:62F:`, or the intermediate `M`).
    pub opening: Option<i64>,
    pub closing: Option<i64>,
    pub currency: String,
    pub lines: Vec<Line>,
}

/// `true` when the text looks like MT940.
pub fn is_mt940(text: &str) -> bool {
    text.contains(":20:") && text.contains(":61:") && (text.contains(":60F:") || text.contains(":60M:"))
}

static MT_TAG: LazyLock<Regex> = LazyLock::new(|| re(r"^:([0-9]{2}[A-Z]?):(.*)$"));
static MT_BAL: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([CD])([0-9]{6})([A-Z]{3})([0-9]+(?:,[0-9]{0,2})?)"));
static MT_61: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^([0-9]{6})([0-9]{4})?(RC|RD|C|D)([A-Z])?([0-9]+,[0-9]{0,2})([A-Z][A-Z0-9]{3})([^/\n]*)(?://([^\n]*))?")
});

fn mt_amount(s: &str) -> Option<i64> {
    let (int, frac) = s.split_once(',').unwrap_or((s, ""));
    let int: i64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut f: String = frac.chars().take(2).collect();
    while f.len() < 2 {
        f.push('0');
    }
    int.checked_mul(100)?.checked_add(f.parse().ok()?)
}

fn mt_date(yymmdd: &str) -> Option<NaiveDate> {
    let y: i32 = yymmdd.get(0..2)?.parse().ok()?;
    let m: u32 = yymmdd.get(2..4)?.parse().ok()?;
    let d: u32 = yymmdd.get(4..6)?.parse().ok()?;
    // SWIFT years are two digits: 80–99 → 19xx.
    NaiveDate::from_ymd_opt(if y >= 80 { 1900 + y } else { 2000 + y }, m, d)
}

fn mt_balance(v: &str) -> Option<(i64, String)> {
    let c = MT_BAL.captures(v.trim())?;
    let amt = mt_amount(&c[4])?;
    Some((if &c[1] == "D" { -amt } else { amt }, c[3].to_string()))
}

/// Parses one MT940 statement (the first, when a file holds several). Lines that do not parse
/// are reported in the second value with their 1-based line number.
pub fn parse_mt940(text: &str) -> (Mt940, Vec<Skipped>) {
    // Fold continuation lines onto their tag.
    let mut fields: Vec<(String, String, usize)> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let l = raw.trim_end();
        if l.starts_with('-') && l.trim_matches(|c| c == '-' || c == '}').is_empty() {
            if fields.iter().any(|(t, _, _)| t == "62F" || t == "62M") {
                break;
            }
            continue;
        }
        if let Some(c) = MT_TAG.captures(l) {
            fields.push((c[1].to_string(), c[2].to_string(), i + 1));
        } else if let Some(last) = fields.last_mut() {
            if !l.trim().is_empty() {
                last.1.push('\n');
                last.1.push_str(l.trim());
            }
        }
    }
    let mut st = Mt940 {
        reference: String::new(),
        account: String::new(),
        statement_no: String::new(),
        opening: None,
        closing: None,
        currency: String::new(),
        lines: Vec::new(),
    };
    let mut skipped = Vec::new();
    for (tag, value, row) in &fields {
        match tag.as_str() {
            "20" => st.reference = value.trim().to_string(),
            "25" => st.account = value.trim().to_string(),
            "28C" | "28" => st.statement_no = value.trim().to_string(),
            "60F" | "60M" => {
                if let Some((b, cur)) = mt_balance(value) {
                    st.opening = Some(b);
                    st.currency = cur;
                }
            }
            "62F" | "62M" => {
                if let Some((b, _)) = mt_balance(value) {
                    st.closing = Some(b);
                }
            }
            "61" => match MT_61.captures(value) {
                Some(c) => {
                    let (Some(date), Some(amt)) = (mt_date(&c[1]), mt_amount(&c[5])) else {
                        skipped.push(Skipped {
                            row: *row,
                            reason: "Unreadable :61: date or amount".into(),
                        });
                        continue;
                    };
                    let into_account = matches!(&c[3], "C" | "RD");
                    let customer_ref = c.get(7).map_or("", |m| m.as_str()).trim();
                    let bank_ref = c.get(8).map_or("", |m| m.as_str()).trim();
                    let supplementary = value.split_once('\n').map_or("", |(_, s)| s).trim();
                    st.lines.push(Line {
                        date,
                        amount: if into_account { amt } else { -amt },
                        desc: squash(supplementary),
                        fitid: bank_ref.to_string(),
                        reference: if customer_ref.eq_ignore_ascii_case("NONREF") {
                            String::new()
                        } else {
                            customer_ref.to_string()
                        },
                    });
                }
                None => skipped.push(Skipped {
                    row: *row,
                    reason: "Unreadable :61: line".into(),
                }),
            },
            "86" => {
                if let Some(last) = st.lines.last_mut() {
                    let text = squash(&value.replace('\n', " "));
                    last.desc = if last.desc.is_empty() {
                        text
                    } else {
                        format!("{text} {}", last.desc)
                    };
                }
            }
            _ => {}
        }
    }
    (st, skipped)
}

// ------------------------------------------------------------------------------- de-dupe

/// A stable identity for a line, for skipping re-imports. Port of `lineIdentity`.
pub fn line_identity(l: &Line) -> String {
    if !l.fitid.is_empty() {
        return format!("f:{}", l.fitid);
    }
    format!("d:{}|{}|{}", l.date, l.amount, squash(&l.desc).to_lowercase())
}

/// Drops lines already imported. Duplicates inside one file are kept, counted against the number
/// already present; a CSV and an OFX download of the same statement match on date and amount.
/// Returns, per new line, whether it is a duplicate. Port of `bankimport.js` `dedupe`.
pub fn dedupe(new_lines: &[Line], existing: &[Line]) -> Vec<bool> {
    let mut exact: HashMap<String, Vec<usize>> = HashMap::new();
    let mut loose: HashMap<String, Vec<usize>> = HashMap::new();
    let loose_key = |l: &Line, fit: bool| format!("{}|{}|{}", u8::from(fit), l.date, l.amount);
    for (i, l) in existing.iter().enumerate() {
        exact.entry(line_identity(l)).or_default().push(i);
        loose
            .entry(loose_key(l, !l.fitid.is_empty()))
            .or_default()
            .push(i);
    }
    let mut used: HashSet<usize> = HashSet::new();
    let mut take = |map: &mut HashMap<String, Vec<usize>>, k: &str, used: &mut HashSet<usize>| {
        if let Some(list) = map.get_mut(k) {
            while let Some(i) = list.pop() {
                if used.insert(i) {
                    return true;
                }
            }
        }
        false
    };
    new_lines
        .iter()
        .map(|l| {
            take(&mut exact, &line_identity(l), &mut used)
                || take(&mut loose, &loose_key(l, l.fitid.is_empty()), &mut used)
        })
        .collect()
}

// ------------------------------------------------------------------------------ detection

/// Statement file formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Csv,
    Ofx,
    Mt940,
}

/// Guesses the format of a statement file from its content.
pub fn detect(text: &str) -> Format {
    if is_ofx(text) {
        Format::Ofx
    } else if is_mt940(text) {
        Format::Mt940
    } else {
        Format::Csv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }
    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    const DMY: CsvOptions = CsvOptions {
        date_style: DateStyle::Dmy,
        invert: false,
    };

    #[test]
    fn money_reads_indian_grouping() {
        assert_eq!(parse_money("1,00,000.50"), 10_000_050);
        assert_eq!(parse_money("₹ 12,34,567"), 123_456_700);
        assert_eq!(parse_money("(250.00)"), -25_000);
        assert_eq!(parse_money("12,5"), 1250);
        assert_eq!(parse_money("10.005"), 1001);
        assert_eq!(parse_money(""), 0);
    }

    #[test]
    fn dates_default_to_day_first_in_india() {
        assert_eq!(parse_statement_date("03/04/2026", DateStyle::Dmy), Some(d(2026, 4, 3)));
        assert_eq!(parse_statement_date("03/04/2026", DateStyle::Mdy), Some(d(2026, 3, 4)));
        assert_eq!(parse_statement_date("04-Mar-26", DateStyle::Dmy), Some(d(2026, 3, 4)));
        assert_eq!(parse_statement_date("31/02/2026", DateStyle::Dmy), None);
    }

    #[test]
    fn hdfc_layout_with_preamble() {
        let csv = "HDFC BANK Ltd.,,,,,,\nAccount No : XXXX1234,,,,,,\n\
Date,Narration,Chq./Ref.No.,Value Dt,Withdrawal Amt.,Deposit Amt.,Closing Balance\n\
01/07/26,NEFT CR-SBIN0001-FEE PAYMENT,N123456789,01/07/26,,\"99,000.00\",\"1,99,000.00\"\n\
02/07/26,CHQ PAID-MSEB,000123,02/07/26,\"12,500.00\",,\"1,86,500.00\"\n";
        let st = parse_csv_statement(csv, None, DMY).unwrap();
        assert_eq!(st.header_rows, 3);
        assert!(st.mapping.split);
        assert_eq!(st.lines.len(), 2, "{:?}", st.skipped);
        assert_eq!(st.lines[0].amount, 9_900_000);
        assert_eq!(st.lines[0].reference, "N123456789");
        assert_eq!(st.lines[1].amount, -1_250_000);
        assert_eq!(st.lines[1].date, d(2026, 7, 2));
        assert!(st.skipped.is_empty());
    }

    #[test]
    fn sbi_style_dr_cr_indicator() {
        let csv = "Txn Date,Description,Ref No./Cheque No.,Amount,Dr / Cr\n\
3 Jul 2026,BY TRANSFER-UPI,UTR77,\"5,000.00\",CR\n\
4 Jul 2026,ATM WDL,,500.00,DR\n";
        let st = parse_csv_statement(csv, None, DMY).unwrap();
        assert_eq!(st.mapping.indicator, 4);
        assert_eq!(
            st.lines.iter().map(|l| l.amount).collect::<Vec<_>>(),
            [500_000, -50_000]
        );
        assert_eq!(st.lines[0].reference, "UTR77");
    }

    #[test]
    fn mt940_statement() {
        let t = ":20:STMT0001\n:25:HDFC0001234/50200012345678\n:28C:00045/001\n\
:60F:C260701INR100000,00\n\
:61:2607020702C99000,00NTRFUTR123//HDFCN26070201\n\
:86:NEFT FEE PAYMENT ROLL 22\nSEM 3\n\
:61:2607030703D12500,NCHK000123//HDFCC26070301\n\
:86:CHQ PAID MSEB\n:62F:C260703INR186500,00\n-";
        let (st, skipped) = parse_mt940(t);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(st.opening, Some(10_000_000));
        assert_eq!(st.closing, Some(18_650_000));
        assert_eq!(st.lines.len(), 2);
        assert_eq!(st.lines[0].amount, 9_900_000);
        assert_eq!(st.lines[0].reference, "UTR123");
        assert_eq!(st.lines[0].fitid, "HDFCN26070201");
        assert_eq!(st.lines[0].desc, "NEFT FEE PAYMENT ROLL 22 SEM 3");
        assert_eq!(st.lines[1].amount, -1_250_000);
        assert_eq!(detect(t), Format::Mt940);
    }

    #[test]
    fn dedupe_matches_csv_against_ofx() {
        let l = |a: i64, fit: &str, desc: &str| Line {
            date: d(2026, 7, 1),
            amount: a,
            desc: desc.into(),
            fitid: fit.into(),
            reference: String::new(),
        };
        let existing = vec![l(100, "F1", "Coffee"), l(100, "", "coffee")];
        let new = vec![l(100, "", "COFFEE"), l(100, "", "Tea"), l(100, "", "x")];
        // first: exact identity; second: loose CSV↔OFX match on F1; third: nothing left.
        assert_eq!(dedupe(&new, &existing), [true, true, false]);
        assert_eq!(guess_mapping(&s(&["Date", "Debit/Credit", "Amount"])).debit, -1);
    }
}
