//! **ERP_EXTENSIONS 5C** — staff time tracking against research projects / sponsored grants,
//! internal projects and committees: the maths in [`logic`] is ported from HeyPuter/everything
//! time-tracking (MIT); [`http`] is the API (weekly timesheets with PI / manager approval,
//! effort reports, CSV import / export).

pub mod http;
pub mod logic;
