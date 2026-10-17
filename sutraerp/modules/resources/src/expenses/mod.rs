//! **ERP_EXTENSIONS 3D** — staff expense claims: receipt capture with OCR pre-fill (PaddleOCR
//! sidecar behind [`ocr::ReceiptOcrPort`]), expenses with category policy and GST, and claims
//! that move draft → submitted → approved → reimbursed, booked to the ledger through the
//! procurement posting port. Operation ids `resources_expense_*`, tag `resources-procurement`.
//!
//! Rules in [`rules`] are ported from HeyPuter/everything `expense` (MIT); reading Indian
//! receipts from OCR text is [`receipt_text`]; the HTTP layer is in [`http`].

pub mod http;
pub mod ocr;
pub mod receipt_text;
pub mod rules;

pub use http::router;
