//! **ERP_EXTENSIONS 3E** — thermal receipt printing at the fee counter (ESC/POS).
//!
//! The server builds every slip (the doc model, the ESC/POS bytes and a plain-text preview), so
//! the paper copy, the on-screen copy and a reprint can never disagree; the browser only carries
//! the bytes to the printer on its counter (Web Serial, WebUSB or Web Bluetooth). Which printer a
//! counter uses is a property of that device and stays in the browser.
//!
//! * [`escpos`] — pure port of Everything's `pos/src/lib/escpos.js` (MIT, commit 06802a1).
//! * [`slip`] — a fee receipt and the counter's day close as slips, amounts in Indian words.
//! * [`http`] — print settings, receipt slips, the test slip and the counter day-close report
//!   (tag `finance-payments`, ops `finance_receipt_*`). Contract:
//!   `docs/extensions-3e-printing.md`.

pub mod escpos;
pub mod http;
pub mod slip;
