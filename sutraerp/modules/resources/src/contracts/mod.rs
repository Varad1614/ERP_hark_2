//! **ERP_EXTENSIONS 3C** — contracts and MoUs: vendor contracts, AMCs, leases, licences and
//! academic / industry / research MoUs with their term clock, recorded renewal decisions, term
//! history, obligations (who owes what, when; repeating on a month cadence), an iCalendar feed of
//! deadlines, and MoU activities rolled up as NAAC evidence. Operation ids `resources_contract_*`,
//! tag `resources-procurement`.
//!
//! Rules in [`rules`] and [`ical`] are ported from HeyPuter/everything `contracts` (MIT); the
//! HTTP layer is in [`http`].

pub mod http;
pub mod ical;
pub mod naac;
pub mod rules;

pub use http::router;
