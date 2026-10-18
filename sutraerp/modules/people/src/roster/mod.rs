//! **ERP_EXTENSIONS 3A** — duty rosters: rules ported from HeyPuter/everything shifts (MIT).
//! **ERP_EXTENSIONS 5B** — shift swaps / covers / open-shift claims, clock punches, timesheets,
//! the payroll export, roster reports and the staff calendar, ported from the same app's
//! `swaps.js`, `attendance.js`, `timesheet.js`, `payroll.js`, `reports.js` and `ics.js`.

pub mod clock;
pub mod http;
pub mod ics;
pub mod reports;
pub mod rules;
pub mod shifts_http;
pub mod swaps;
pub mod timesheet;
