//! **ERP_EXTENSIONS 5D** — GST tax invoices, bills of supply and GST credit notes.
//!
//! * [`rules`] — pure: Everything's invoicing document arithmetic (`docTotals`, tax notes,
//!   document numbers; HeyPuter/everything @ 06802a1, MIT) with GST on top (CGST + SGST or
//!   IGST by place of supply, slabs, exempt / nil / non-GST, FY series, credit-note reversal,
//!   GSTR-1 friendly summary). Golden vectors: `tests/golden/gst_invoicing.json`.
//! * [`repo`] — the GST snapshot of a charge (hooked into `shared::charge::insert_charge`),
//!   numbering on publication (`charges::repo::publish_locked`) and GST credit notes
//!   (`adjustments::repo`).
//! * [`http`] — GST profile, fee-head tax setup, payer GST details, printable tax documents
//!   (staff and the student's own), and the GST summary as JSON / CSV (tag `finance-gst`, ops
//!   `finance_gst_*`). Contract: `docs/extensions-5d-gst.md`.

pub mod http;
pub mod repo;
pub mod rules;

pub use http::router;
