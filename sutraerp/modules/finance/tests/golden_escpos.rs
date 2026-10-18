//! Golden vectors for `printing::escpos`, generated from Everything's original
//! `pos/src/lib/escpos.js` (encodeCp437, printWidth, wrap, EscposBuilder, escposReceipt,
//! escposReport, drawerKickBytes) by `tools/golden/generate.mjs`.
//!
//! The inputs never contain `₹`: the port prints it as `Rs.` where the original prints `INR`
//! (covered by the module's unit tests instead).

use serde::Deserialize;
use sutra_finance::printing::escpos::{
    drawer_kick_bytes, encode_cp437, escpos_receipt, escpos_report, print_width, wrap,
    EscposBuilder, ReportHead, ReportRow, SlipDoc,
};

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    input: Input,
    expected: Expected,
}

#[derive(Deserialize)]
struct Input {
    texts: Vec<String>,
    wraps: Vec<(String, usize)>,
    builder: Builder,
    receipt: Receipt,
    report: Report,
    connector: u8,
}

#[derive(Deserialize)]
struct Builder {
    columns: usize,
    ops: Vec<Op>,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
enum Op {
    ColumnsLine { label: String, amount: String, indent: usize },
    Wrapped { value: String, indent: usize, scale: usize },
}

#[derive(Deserialize)]
struct Receipt {
    doc: SlipDoc,
    columns: usize,
    kick: bool,
}

#[derive(Deserialize)]
struct Report {
    head: ReportHead,
    rows: Vec<ReportRow>,
    columns: usize,
}

#[derive(Deserialize)]
struct Expected {
    cp437: Vec<Vec<u8>>,
    widths: Vec<usize>,
    wraps: Vec<Vec<String>>,
    builder: Vec<u8>,
    receipt: Vec<u8>,
    report: Vec<u8>,
    kick: Vec<u8>,
}

#[test]
fn escpos_matches_everything() {
    let f: Fixture =
        serde_json::from_str(include_str!("golden/escpos.json")).expect("valid fixture");
    assert_eq!(f.cases.len(), 200);
    for (i, c) in f.cases.iter().enumerate() {
        let (inp, exp) = (&c.input, &c.expected);
        let cp437: Vec<Vec<u8>> = inp.texts.iter().map(|t| encode_cp437(t)).collect();
        assert_eq!(cp437, exp.cp437, "case {i}: cp437 of {:?}", inp.texts);
        let widths: Vec<usize> = inp.texts.iter().map(|t| print_width(t)).collect();
        assert_eq!(widths, exp.widths, "case {i}: widths of {:?}", inp.texts);
        let wraps: Vec<Vec<String>> = inp.wraps.iter().map(|(t, w)| wrap(t, *w)).collect();
        assert_eq!(wraps, exp.wraps, "case {i}: wraps of {:?}", inp.wraps);

        let mut b = EscposBuilder::new(inp.builder.columns);
        for op in &inp.builder.ops {
            match op {
                Op::ColumnsLine { label, amount, indent } => {
                    b.columns_line(label, amount, *indent);
                }
                Op::Wrapped { value, indent, scale } => {
                    b.wrapped(value, *indent, *scale);
                }
            }
        }
        assert_eq!(b.build(), exp.builder, "case {i}: builder");

        let r = &inp.receipt;
        assert_eq!(escpos_receipt(&r.doc, r.columns, r.kick), exp.receipt, "case {i}: receipt");
        let rep = &inp.report;
        let got = escpos_report(&rep.head, &rep.rows, rep.columns);
        assert_eq!(got, exp.report, "case {i}: report");
        assert_eq!(drawer_kick_bytes(inp.connector), exp.kick, "case {i}: kick");
    }
}
