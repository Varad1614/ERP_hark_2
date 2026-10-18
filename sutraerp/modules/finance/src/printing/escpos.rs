//! ESC/POS: a fee receipt as bytes a thermal printer understands.
//!
//! Faithful port of HeyPuter/everything `src/pos/src/lib/escpos.js` at commit 06802a1 (MIT).
//! No vendor SDK: the command set every till printer has spoken since the eighties is a handful
//! of escape sequences, and the browser's own transports (Web Serial, WebUSB, Web Bluetooth) can
//! carry them. This module is pure: it turns a [`SlipDoc`] into bytes, so the on-screen copy,
//! the plain-text preview ([`readable`]) and the paper slip can't drift apart.
//!
//! Layout is monospaced columns rather than pixels. A printer's line is 32 characters on 58 mm
//! paper and 48 on 80 mm; a label and an amount are printed as one line padded between them.
//!
//! Indianisation (deliberate divergence from the JS, kept out of the golden inputs): `₹` prints
//! as `Rs.` (JS: `INR`). Devanagari has no CP437 bytes and prints `?`.

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use utoipa::ToSchema;

/// The two paper widths a counter actually buys: `(columns, label)`.
pub const PAPER_WIDTHS: [(usize, &str); 2] =
    [(32, "58 mm · 32 characters"), (48, "80 mm · 48 characters")];

const ESC: u8 = 0x1b;
const GS: u8 = 0x1d;

/// Code page 437, the default character set of practically every ESC/POS printer. Only the
/// entries a slip can realistically carry are listed; anything else prints `?`, because a byte
/// the printer maps to a different glyph would be a wrong receipt, quietly.
const CP437: [(char, u8); 49] = [
    ('Ç', 0x80),
    ('ü', 0x81),
    ('é', 0x82),
    ('â', 0x83),
    ('ä', 0x84),
    ('à', 0x85),
    ('å', 0x86),
    ('ç', 0x87),
    ('ê', 0x88),
    ('ë', 0x89),
    ('è', 0x8a),
    ('ï', 0x8b),
    ('î', 0x8c),
    ('ì', 0x8d),
    ('Ä', 0x8e),
    ('Å', 0x8f),
    ('É', 0x90),
    ('æ', 0x91),
    ('Æ', 0x92),
    ('ô', 0x93),
    ('ö', 0x94),
    ('ò', 0x95),
    ('û', 0x96),
    ('ù', 0x97),
    ('ÿ', 0x98),
    ('Ö', 0x99),
    ('Ü', 0x9a),
    ('¢', 0x9b),
    ('£', 0x9c),
    ('¥', 0x9d),
    ('₧', 0x9e),
    ('ƒ', 0x9f),
    ('á', 0xa0),
    ('í', 0xa1),
    ('ó', 0xa2),
    ('ú', 0xa3),
    ('ñ', 0xa4),
    ('Ñ', 0xa5),
    ('ª', 0xa6),
    ('º', 0xa7),
    ('¿', 0xa8),
    ('ß', 0xe1),
    ('µ', 0xe6),
    ('±', 0xf1),
    ('÷', 0xf6),
    ('°', 0xf8),
    ('·', 0xfa),
    ('²', 0xfd),
    ('\u{a0}', 0x20),
];

/// Typography spelled the way a till printer can print it: minus signs, dashes, curly quotes,
/// and the currency marks that have no CP437 byte. `¥` is absent on purpose (it has byte 0x9d).
/// `₹` is `Rs.` here (the JS original prints `INR`).
fn transliterate(ch: char) -> Option<&'static str> {
    Some(match ch {
        '\u{2212}' | '\u{2013}' | '\u{2014}' => "-",
        '\u{2018}' | '\u{2019}' => "'",
        '\u{201c}' | '\u{201d}' => "\"",
        '\u{2026}' => "...",
        '\u{d7}' => "x",
        '\u{20ac}' => "EUR",
        '\u{20b9}' => "Rs.",
        '\u{20a9}' => "KRW",
        '\u{20ba}' => "TRY",
        '\u{20bd}' => "RUB",
        '\u{2022}' => "*",
        '\u{2192}' => "->",
        '\u{2191}' => "^",
        '\u{2193}' => "v",
        _ => return None,
    })
}

fn cp437_byte(ch: char) -> Option<u8> {
    CP437.iter().find(|(c, _)| *c == ch).map(|(_, b)| *b)
}

/// One byte per printable character, in code page 437.
///
/// The text is NFC-normalised first, so `e` plus a combining acute prints as `é` rather than
/// `e?`. Transliterated characters expand (`€` → `EUR`); anything unmapped becomes `?`.
pub fn encode_cp437(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for ch in text.nfc() {
        if let Some(swap) = transliterate(ch) {
            out.extend_from_slice(swap.as_bytes());
            continue;
        }
        let code = u32::from(ch);
        if code == 0x0a || (0x20..=0x7e).contains(&code) {
            out.push(code as u8);
            continue;
        }
        out.push(cp437_byte(ch).unwrap_or(b'?'));
    }
    out
}

/// How many columns a string takes on the paper: its encoded length, not its character count
/// (`€24.68` is six characters and eight columns).
pub fn print_width(text: &str) -> usize {
    encode_cp437(text).len()
}

fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    print_width(ch.encode_utf8(&mut buf))
}

/// JavaScript's `\s` class, so words split exactly as the original's `/\s+/` does.
fn is_js_space(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// Greedy word wrap to `width` printed columns; a word longer than the line is broken rather
/// than lost. Always returns at least one row, and drops trailing empty paragraphs.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    // As much of `row` as fits one line, by column; at least one character so it progresses.
    let head = |row: &str| -> String {
        let mut out = String::new();
        let mut w = 0;
        for ch in row.chars() {
            let cw = char_width(ch);
            if w + cw > width {
                break;
            }
            out.push(ch);
            w += cw;
        }
        if out.is_empty() {
            row.chars().next().map(String::from).unwrap_or_default()
        } else {
            out
        }
    };
    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut row = String::new();
        for word in paragraph.split(is_js_space).filter(|w| !w.is_empty()) {
            if row.is_empty() {
                row = word.to_string();
            } else if print_width(&row) + 1 + print_width(word) <= width {
                row.push(' ');
                row.push_str(word);
            } else {
                out.push(std::mem::replace(&mut row, word.to_string()));
            }
            while print_width(&row) > width {
                let fits = head(&row);
                let taken = fits.chars().count();
                out.push(fits);
                row = row.chars().skip(taken).collect();
            }
        }
        out.push(row);
    }
    while out.len() > 1 && out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Shorten to fit `max` columns, marking the cut with `~` rather than just stopping. Measured
/// in printed columns, so a name with a transliterated character is cut where the paper ends.
pub fn clip(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if print_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let w = char_width(ch);
        if width + w > max - 1 {
            break;
        }
        out.push(ch);
        width += w;
    }
    out.push('~');
    out
}

/// A byte buffer with the handful of ESC/POS commands a receipt needs. Each helper sets what it
/// needs; the document builders put alignment and emphasis back.
#[derive(Debug, Clone)]
pub struct EscposBuilder {
    columns: usize,
    bytes: Vec<u8>,
}

impl Default for EscposBuilder {
    fn default() -> Self {
        Self::new(48)
    }
}

impl EscposBuilder {
    /// A builder for paper `columns` characters wide (32 on 58 mm, 48 on 80 mm).
    pub fn new(columns: usize) -> Self {
        Self {
            columns,
            bytes: Vec::new(),
        }
    }

    /// The paper width this builder lays out for.
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// Append raw bytes.
    pub fn raw(&mut self, values: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(values);
        self
    }

    /// `ESC @`: forget whatever the last job left set.
    pub fn init(&mut self) -> &mut Self {
        self.raw(&[ESC, 0x40])
    }

    /// `ESC a n`: 0 left, 1 center, 2 right.
    pub fn align(&mut self, n: u8) -> &mut Self {
        self.raw(&[ESC, 0x61, n])
    }

    /// `ESC E n`: emphasis on or off.
    pub fn bold(&mut self, on: bool) -> &mut Self {
        self.raw(&[ESC, 0x45, u8::from(on)])
    }

    /// `GS ! n`: double width and height, for the line that carries the institution's name.
    pub fn big(&mut self, on: bool) -> &mut Self {
        self.raw(&[GS, 0x21, if on { 0x11 } else { 0x00 }])
    }

    /// Text in code page 437, no newline.
    pub fn text(&mut self, value: &str) -> &mut Self {
        self.bytes.extend(encode_cp437(value));
        self
    }

    /// Text followed by a newline.
    pub fn line(&mut self, value: &str) -> &mut Self {
        self.text(value).raw(b"\n")
    }

    /// `ESC d n`: feed `n` lines (clamped to 255).
    pub fn feed(&mut self, n: usize) -> &mut Self {
        self.raw(&[ESC, 0x64, n.min(255) as u8])
    }

    /// A label on the left and an amount on the right, on one line. The label gives way when
    /// the two don't fit: an amount cut short is a wrong receipt.
    pub fn columns_line(&mut self, label: &str, amount: &str, indent: usize) -> &mut Self {
        let right_width = print_width(amount) as i64;
        let room = self.columns as i64 - right_width - indent as i64;
        let pad = " ".repeat(indent);
        if room <= 1 {
            let whole: String = format!("{pad}{amount}")
                .chars()
                .take(self.columns)
                .collect();
            return self.line(&whole);
        }
        let left = clip(label, (room - 1) as usize);
        let gap = self.columns as i64 - indent as i64 - print_width(&left) as i64 - right_width;
        let spaces = " ".repeat(gap.max(1) as usize);
        self.line(&format!("{pad}{left}{spaces}{amount}"))
    }

    /// A paragraph wrapped to the paper on word boundaries. `scale` is the character width the
    /// printer is set to: [`EscposBuilder::big`] doubles it, so only half as many glyphs fit.
    pub fn wrapped(&mut self, value: &str, indent: usize, scale: usize) -> &mut Self {
        let scale = scale.max(1) as i64;
        let width = ((self.columns as i64 - indent as i64 * scale).div_euclid(scale)).max(1);
        let pad = " ".repeat(indent);
        for row in wrap(value, width as usize) {
            self.line(&format!("{pad}{row}"));
        }
        self
    }

    /// A full-width rule of `ch`.
    pub fn rule(&mut self, ch: &str) -> &mut Self {
        let line = ch.repeat(self.columns);
        self.line(&line)
    }

    /// `GS V 66 0`: cut, after feeding the paper clear of the head.
    pub fn cut(&mut self) -> &mut Self {
        self.feed(3).raw(&[GS, 0x56, 66, 0])
    }

    /// The bytes written so far.
    pub fn build(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// The standard drawer pulse: `ESC p`, connector 0 or 1, 50 ms on, 500 ms off. The drawer is
/// wired to the printer, so opening it is a print job with no paper.
pub fn drawer_kick_bytes(connector: u8) -> Vec<u8> {
    vec![ESC, 0x70, u8::from(connector != 0), 25, 250]
}

/// A receipt as the slip shows it: header, meta lines, items, totals, payments, footer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ReceiptSlipDoc)]
pub struct SlipDoc {
    /// Institution name, printed double size.
    pub store_name: String,
    /// Postal address under the name (omitted when empty).
    pub address: String,
    /// Registration lines, e.g. `GSTIN 27AAAAA0000A1Z5`.
    pub registrations: Vec<String>,
    /// Prints `*** VOID ***` under the header.
    pub voided: bool,
    /// Prints `*** {kind} ***` (e.g. `DUPLICATE`) when present and non-empty.
    pub kind: Option<String>,
    /// Receipt number, date, student and similar lines.
    pub meta: Vec<String>,
    /// Line items.
    pub items: Vec<SlipItem>,
    /// Total rows.
    pub totals: Vec<SlipRow>,
    /// Payment rows (a rule is printed above them only when there are some).
    pub payments: Vec<SlipRow>,
    /// Free note (omitted when empty).
    pub note: String,
    /// Centered line (omitted when empty).
    pub reward: String,
    /// Centered bold line (omitted when empty).
    pub loyalty: String,
    /// Centered footer (omitted when empty).
    pub footer: String,
}

/// One item line: quantity and name on the left, amount on the right, sub-lines indented.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ReceiptSlipItem)]
pub struct SlipItem {
    /// Quantity text, e.g. `1x`.
    pub qty: String,
    /// Item name; shortened with `~` when the paper is narrow.
    pub name: String,
    /// Amount text, never shortened.
    pub amount: String,
    /// Indented detail lines.
    pub subs: Vec<String>,
}

/// A label/value row (totals and payments).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ReceiptSlipRow)]
pub struct SlipRow {
    /// Left-hand label.
    pub label: String,
    /// Right-hand value.
    pub value: String,
    /// Printed bold.
    #[serde(default)]
    pub strong: bool,
}

/// Header of a counter (day-close) report.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = CounterReportHead)]
pub struct ReportHead {
    /// Institution name, printed double size.
    pub store_name: String,
    /// Address (omitted when empty).
    pub address: String,
    /// Report kind, printed as `*** {kind} ***`.
    pub kind: String,
    /// Date, counter and similar lines.
    pub meta: Vec<String>,
    /// Centered footer (omitted when empty).
    pub footer: String,
}

/// One report row: a section heading when `section` is set, else a label/value line.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = CounterReportRow)]
pub struct ReportRow {
    /// Left-hand label.
    pub label: String,
    /// Right-hand value.
    pub value: String,
    /// Section heading (printed upper-case after a rule); the row's label/value are ignored.
    pub section: Option<String>,
    /// Printed bold.
    #[serde(default)]
    pub strong: bool,
}

fn rows(b: &mut EscposBuilder, list: &[SlipRow]) {
    for row in list {
        if row.strong {
            b.bold(true);
        }
        b.columns_line(&row.label, &row.value, 0);
        if row.strong {
            b.bold(false);
        }
    }
}

/// A [`SlipDoc`] as ESC/POS bytes for paper `columns` wide, ending with the cut and, when
/// `kick` is set, the drawer pulse (after the slip is out).
pub fn escpos_receipt(doc: &SlipDoc, columns: usize, kick: bool) -> Vec<u8> {
    let mut b = EscposBuilder::new(columns);
    b.init().align(1);
    b.bold(true)
        .big(true)
        .wrapped(&doc.store_name, 0, 2)
        .big(false)
        .bold(false);
    if !doc.address.is_empty() {
        b.wrapped(&doc.address, 0, 1);
    }
    for reg in &doc.registrations {
        b.wrapped(reg, 0, 1);
    }
    if doc.voided {
        b.feed(1).bold(true).line("*** VOID ***").bold(false);
    }
    if let Some(kind) = doc.kind.as_deref().filter(|k| !k.is_empty()) {
        b.feed(1)
            .bold(true)
            .line(&format!("*** {kind} ***"))
            .bold(false);
    }
    b.feed(1);
    for meta in &doc.meta {
        b.wrapped(meta, 0, 1);
    }

    b.align(0).rule("-");
    for item in &doc.items {
        b.columns_line(&format!("{} {}", item.qty, item.name), &item.amount, 0);
        for sub in &item.subs {
            b.wrapped(sub, 2, 1);
        }
    }

    b.rule("-");
    rows(&mut b, &doc.totals);
    if !doc.payments.is_empty() {
        b.rule("-");
        rows(&mut b, &doc.payments);
    }

    if !doc.note.is_empty() {
        b.rule("-").wrapped(&doc.note, 0, 1);
    }
    if !doc.reward.is_empty() || !doc.loyalty.is_empty() {
        b.rule("-").align(1);
        if !doc.reward.is_empty() {
            b.wrapped(&doc.reward, 0, 1);
        }
        if !doc.loyalty.is_empty() {
            b.bold(true).wrapped(&doc.loyalty, 0, 1).bold(false);
        }
        b.align(0);
    }
    if !doc.footer.is_empty() {
        b.rule("-").align(1).wrapped(&doc.footer, 0, 1).align(0);
    }

    b.cut();
    if kick {
        b.raw(&drawer_kick_bytes(0));
    }
    b.build()
}

/// A counter report (day close) as ESC/POS bytes, rows in the order given.
pub fn escpos_report(head: &ReportHead, rows: &[ReportRow], columns: usize) -> Vec<u8> {
    let mut b = EscposBuilder::new(columns);
    b.init().align(1);
    b.bold(true)
        .big(true)
        .wrapped(&head.store_name, 0, 2)
        .big(false)
        .bold(false);
    if !head.address.is_empty() {
        b.wrapped(&head.address, 0, 1);
    }
    b.feed(1)
        .bold(true)
        .line(&format!("*** {} ***", head.kind))
        .bold(false);
    for meta in &head.meta {
        b.wrapped(meta, 0, 1);
    }
    b.align(0);

    for row in rows {
        if let Some(section) = row.section.as_deref().filter(|s| !s.is_empty()) {
            b.rule("-")
                .bold(true)
                .line(&section.to_uppercase())
                .bold(false);
            continue;
        }
        if row.strong {
            b.bold(true);
        }
        b.columns_line(&row.label, &row.value, 0);
        if row.strong {
            b.bold(false);
        }
    }

    if !head.footer.is_empty() {
        b.rule("-").align(1).wrapped(&head.footer, 0, 1).align(0);
    }
    b.cut();
    b.build()
}

/// The text a job would print, for a plain-text preview: the builder's commands (`ESC @`,
/// `ESC a|E|d n`, `ESC p m t1 t2`, `GS ! n`, `GS V m n`) are parsed and dropped, newlines and
/// printable ASCII pass through, and CP437 high bytes map back to Unicode (`?` if unknown).
/// Feeds are dropped rather than rendered as blank lines.
pub fn readable(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            ESC => {
                i += match bytes.get(i + 1) {
                    Some(0x40) => 2,
                    Some(0x70) => 5,
                    _ => 3,
                };
                continue;
            }
            GS => {
                i += if bytes.get(i + 1) == Some(&0x56) {
                    4
                } else {
                    3
                };
                continue;
            }
            0x0a => out.push('\n'),
            0x20..=0x7e => out.push(char::from(b)),
            0x80..=0xff => {
                let ch = CP437
                    .iter()
                    .find(|(c, code)| *code == b && *c != '\u{a0}')
                    .map_or('?', |(c, _)| *c);
                out.push(ch);
            }
            _ => {}
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(text: &str) -> Vec<u8> {
        text.bytes().collect()
    }

    fn doc() -> SlipDoc {
        SlipDoc {
            store_name: "Corner Roast Café".into(),
            address: "12 Market Lane".into(),
            registrations: vec!["VAT No. GB123".into()],
            kind: None,
            voided: false,
            meta: vec!["Receipt #1042".into(), "Thursday, August 20, 2026".into()],
            items: vec![SlipItem {
                qty: "2×".into(),
                name: "Espresso".into(),
                amount: "$6.40".into(),
                subs: vec!["no ice".into()],
            }],
            totals: vec![SlipRow {
                label: "Total".into(),
                value: "$7.01".into(),
                strong: true,
            }],
            payments: vec![
                SlipRow {
                    label: "Cash".into(),
                    value: "$10.00".into(),
                    strong: false,
                },
                SlipRow {
                    label: "Change".into(),
                    value: "$2.99".into(),
                    strong: false,
                },
            ],
            note: "for Sam".into(),
            reward: "a free espresso".into(),
            loyalty: "1 of 3 toward a free espresso".into(),
            footer: "Thanks, see you tomorrow".into(),
        }
    }

    #[test]
    fn ascii_passes_through() {
        assert_eq!(encode_cp437("Latte $4.60"), codes("Latte $4.60"));
    }

    #[test]
    fn accents_map_and_decomposed_composes() {
        assert_eq!(encode_cp437("Café"), vec![0x43, 0x61, 0x66, 0x82]);
        assert_eq!(encode_cp437("Cafe\u{301}"), vec![0x43, 0x61, 0x66, 0x82]);
    }

    #[test]
    fn transliterates_typography() {
        assert_eq!(encode_cp437("\u{2212}7.01"), codes("-7.01"));
        assert_eq!(encode_cp437("·"), vec![0xfa]);
        assert_eq!(encode_cp437("€5"), codes("EUR5"));
        assert_eq!(encode_cp437("it\u{2019}s"), codes("it's"));
    }

    #[test]
    fn yen_has_its_own_byte() {
        let mut want = codes("CN");
        want.push(0x9d);
        want.extend(codes("12.50"));
        assert_eq!(encode_cp437("CN¥12.50"), want);
        assert_eq!(print_width("¥13"), 3);
    }

    #[test]
    fn unknown_is_question_mark() {
        assert_eq!(encode_cp437("日"), vec![b'?']);
        assert_eq!(encode_cp437("पुणे"), codes("????"));
    }

    #[test]
    fn rupee_prints_as_rs() {
        assert_eq!(encode_cp437("₹1,250.00"), codes("Rs.1,250.00"));
        assert_eq!(print_width("₹5"), 4);
        let mut b = EscposBuilder::new(20);
        b.columns_line("Tuition fee", "₹12,500.00", 0);
        assert_eq!(readable(&b.build()), "Tuitio~ Rs.12,500.00\n");
    }

    #[test]
    fn widths_count_columns() {
        assert_eq!(print_width("Latte"), 5);
        assert_eq!(print_width("\u{20ac}24.68"), 8);
        assert_eq!(print_width("\u{20a9}1,250"), 8);
        assert_eq!(print_width("Caf\u{e9}"), 4);
        assert_eq!(print_width("\u{2212}7.01"), 5);
        assert_eq!(print_width("more\u{2026}"), 7);
    }

    #[test]
    fn wrap_breaks_words_and_long_words() {
        assert_eq!(
            wrap("one two three four", 9),
            vec!["one two", "three", "four"]
        );
        assert_eq!(wrap("aaaaaaaaaaaa", 5), vec!["aaaaa", "aaaaa", "aa"]);
        assert_eq!(wrap("", 10), vec![""]);
        assert_eq!(wrap("ab \u{20ac}5", 6), vec!["ab", "\u{20ac}5"]);
        for row in wrap("Thank you\u{2026} see you for \u{20ac}5 off", 20) {
            assert!(print_width(&row) <= 20);
        }
    }

    #[test]
    fn pads_label_and_amount() {
        let mut b = EscposBuilder::new(20);
        b.columns_line("Subtotal", "3.20", 0);
        assert_eq!(readable(&b.build()), "Subtotal        3.20\n");
    }

    #[test]
    fn pads_by_printed_columns() {
        for columns in [32, 48] {
            let mut b = EscposBuilder::new(columns);
            b.columns_line("Total", "\u{20ac}24.68", 0);
            b.columns_line("Sales tax 7.75%", "\u{20ac}1.24", 0);
            b.columns_line(
                "A product name long enough to need shortening",
                "\u{20ac}9.99",
                0,
            );
            let text = readable(&b.build());
            for line in text.split('\n').filter(|l| !l.is_empty()) {
                assert_eq!(line.chars().count(), columns);
                assert!(line.ends_with(".68") || line.ends_with(".24") || line.ends_with(".99"));
            }
        }
    }

    #[test]
    fn shortens_label_never_amount() {
        let mut b = EscposBuilder::new(16);
        b.columns_line("A very long product name", "124.00", 0);
        let text = readable(&b.build());
        let line = text.trim();
        assert_eq!(line.len(), 16);
        assert!(line.ends_with("124.00"));
        assert!(line.contains('~'));
    }

    #[test]
    fn cut_feeds_first() {
        let mut b = EscposBuilder::default();
        b.cut();
        assert_eq!(b.build(), vec![0x1b, 0x64, 3, 0x1d, 0x56, 66, 0]);
    }

    #[test]
    fn receipt_keeps_on_screen_order() {
        let text = readable(&escpos_receipt(&doc(), 32, false));
        let at = |needle: &str| text.find(needle).map_or(-1, |i| i as i64);
        assert!(at("Corner Roast") > -1);
        let order = [
            "Corner Roast",
            "12 Market Lane",
            "VAT No. GB123",
            "Receipt #1042",
            "Espresso",
            "no ice",
            "Total",
            "Change",
            "for Sam",
            "1 of 3 toward",
            "Thanks, see you tomorrow",
        ];
        for pair in order.windows(2) {
            assert!(at(pair[1]) > at(pair[0]), "{} after {}", pair[1], pair[0]);
        }
        let item = text
            .lines()
            .find(|l| l.starts_with("2x Espresso"))
            .unwrap_or_default();
        assert!(item.ends_with("$6.40"));
    }

    #[test]
    fn marks_void_and_kind() {
        let voided = SlipDoc {
            voided: true,
            ..doc()
        };
        assert!(readable(&escpos_receipt(&voided, 32, false)).contains("*** VOID ***"));
        let refund = SlipDoc {
            kind: Some("REFUND".into()),
            ..doc()
        };
        assert!(readable(&escpos_receipt(&refund, 32, false)).contains("*** REFUND ***"));
    }

    #[test]
    fn lines_hold_to_width_and_store_name_wraps_at_half() {
        for columns in [32, 48] {
            let text = readable(&escpos_receipt(&doc(), columns, false));
            assert!(text.lines().all(|l| l.chars().count() <= columns));
        }
        let long = SlipDoc {
            store_name: "The Corner Roast Coffee House".into(),
            ..doc()
        };
        let narrow = readable(&escpos_receipt(&long, 32, false));
        let narrow: Vec<&str> = narrow.split('\n').collect();
        assert_eq!(narrow[0], "The Corner Roast");
        assert_eq!(narrow[1], "Coffee House");
        let wide = readable(&escpos_receipt(&long, 48, false));
        let wide: Vec<&str> = wide.split('\n').collect();
        assert_eq!(wide[0], "The Corner Roast Coffee");
        assert_eq!(wide[1], "House");
    }

    #[test]
    fn euro_amounts_stay_whole() {
        let row = |label: &str, value: &str, strong| SlipRow {
            label: label.into(),
            value: value.into(),
            strong,
        };
        let euro = SlipDoc {
            items: vec![SlipItem {
                qty: "2x".into(),
                name: "Espresso".into(),
                amount: "\u{20ac}6.40".into(),
                subs: vec![],
            }],
            totals: vec![
                row("Subtotal", "\u{20ac}6.40", false),
                row("Total", "€7.01", true),
            ],
            payments: vec![row("Cash", "€10.00", false), row("Change", "€2.99", false)],
            footer: "Bedankt\u{2026} tot morgen".into(),
            ..doc()
        };
        for columns in [32, 48] {
            let text = readable(&escpos_receipt(&euro, columns, false));
            assert!(text.lines().all(|l| l.chars().count() <= columns));
            assert!(text.contains("EUR7.01"));
        }
    }

    #[test]
    fn ends_with_cut_and_optional_kick() {
        let plain = escpos_receipt(&doc(), 32, false);
        let kicked = escpos_receipt(&doc(), 32, true);
        assert_eq!(plain[plain.len() - 4..], [0x1d, 0x56, 66, 0]);
        assert_eq!(kicked[kicked.len() - 5..], drawer_kick_bytes(0)[..]);
    }

    #[test]
    fn report_sections_and_rows() {
        let head = ReportHead {
            store_name: "Shop".into(),
            address: String::new(),
            kind: "Z-REPORT".into(),
            meta: vec!["Aug 20".into()],
            footer: "counted and closed".into(),
        };
        let rows = vec![
            ReportRow {
                section: Some("Takings".into()),
                ..ReportRow::default()
            },
            ReportRow {
                label: "Cash".into(),
                value: "120.00".into(),
                ..ReportRow::default()
            },
            ReportRow {
                label: "Expected".into(),
                value: "140.00".into(),
                strong: true,
                section: None,
            },
        ];
        let text = readable(&escpos_report(&head, &rows, 32));
        assert!(text.contains("*** Z-REPORT ***"));
        assert!(text.contains("TAKINGS"));
        assert!(text
            .lines()
            .any(|l| l.starts_with("Cash ") && l.ends_with("120.00")));
        assert!(text.contains("counted and closed"));
    }

    #[test]
    fn drawer_pulse_is_standard() {
        assert_eq!(drawer_kick_bytes(0), vec![0x1b, 0x70, 0, 25, 250]);
        assert_eq!(drawer_kick_bytes(1), vec![0x1b, 0x70, 1, 25, 250]);
    }

    #[test]
    fn readable_maps_high_bytes_back() {
        let mut b = EscposBuilder::new(32);
        b.init().bold(true).line("Café · ¥5").bold(false).cut();
        b.raw(&drawer_kick_bytes(0));
        assert_eq!(readable(&b.build()), "Café · ¥5\n");
        assert_eq!(readable(&encode_cp437("a\u{a0}b")), "a b");
    }
}
