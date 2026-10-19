//! iCalendar output (RFC 5545) for contract deadlines: notice deadlines, term ends and a bounded
//! run of renewals, and dated obligations. Ported from HeyPuter/everything
//! `contracts/src/lib/ical.js` (MIT, pinned 06802a1); see THIRD_PARTY_NOTICES.md.
//!
//! Lines fold at 75 octets (UTF-8, never inside a character), text is escaped, all-day events
//! carry an exclusive DTEND, and UIDs are stable so re-importing updates instead of duplicating.

use super::rules::{
    add_days, add_months, days_between, format_date, format_inr, occurrence_due, renewal_info,
    repeat_months, within_series, Contract, DecisionKind, Obligation, Owner,
};
use chrono::{DateTime, NaiveDate, Utc};

pub const PRODID: &str = "-//SutraERP//Contracts and MoUs//EN";
/// Reminders before each deadline, in days.
pub const DEFAULT_ALARMS: [i64; 2] = [7, 1];
/// UID domain.
pub const UID_HOST: &str = "contracts.sutraerp";

/// Escape a TEXT value: backslash, semicolon, comma and newlines.
pub fn escape_text(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Fold a content line to 75 octets, continuing with a leading space.
pub fn fold_line(line: &str) -> String {
    if line.len() <= 75 {
        return line.to_owned();
    }
    let mut parts = vec![];
    let mut cur = String::new();
    let mut bytes = 0;
    for ch in line.chars() {
        let size = ch.len_utf8();
        if bytes + size > 75 {
            parts.push(std::mem::take(&mut cur));
            cur.push(' ');
            bytes = 1;
        }
        cur.push(ch);
        bytes += size;
    }
    parts.push(cur);
    parts.join("\r\n")
}

pub fn ics_date(d: NaiveDate) -> String {
    d.format("%Y%m%d").to_string()
}

pub fn ics_stamp(at: DateTime<Utc>) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// SEQUENCE rises with every edit: minutes since 2020-01-01 UTC.
pub fn sequence_for(updated_at_ms: i64) -> i64 {
    ((updated_at_ms - 1_577_836_800_000) / 60_000).max(0)
}

/// One all-day event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Stable id parts (joined with `-`): contract id, kind, date…
    pub key: Vec<String>,
    pub date: NaiveDate,
    pub sequence: i64,
    pub summary: String,
    pub description: String,
    pub url: String,
    pub categories: Vec<String>,
    pub alarms: Vec<i64>,
}

impl Event {
    pub fn uid(&self) -> String {
        let parts: Vec<&str> = self
            .key
            .iter()
            .map(String::as_str)
            .filter(|s| !s.is_empty())
            .collect();
        format!("sutra-{}@{UID_HOST}", parts.join("-"))
    }
}

fn event_lines(e: &Event, stamp: &str, out: &mut Vec<String>) {
    out.push("BEGIN:VEVENT".into());
    out.push(format!("UID:{}", e.uid()));
    out.push(format!("DTSTAMP:{stamp}"));
    out.push(format!("SEQUENCE:{}", e.sequence));
    out.push(format!("DTSTART;VALUE=DATE:{}", ics_date(e.date)));
    out.push(format!(
        "DTEND;VALUE=DATE:{}",
        ics_date(add_days(e.date, 1))
    ));
    out.push(format!("SUMMARY:{}", escape_text(&e.summary)));
    out.push("TRANSP:TRANSPARENT".into());
    if !e.description.is_empty() {
        out.push(format!("DESCRIPTION:{}", escape_text(&e.description)));
    }
    if !e.url.is_empty() {
        out.push(format!("URL:{}", escape_text(&e.url)));
    }
    if !e.categories.is_empty() {
        let c: Vec<String> = e.categories.iter().map(|c| escape_text(c)).collect();
        out.push(format!("CATEGORIES:{}", c.join(",")));
    }
    for days in &e.alarms {
        out.push("BEGIN:VALARM".into());
        out.push("ACTION:DISPLAY".into());
        out.push(format!("TRIGGER:-P{days}D"));
        out.push(format!("DESCRIPTION:{}", escape_text(&e.summary)));
        out.push("END:VALARM".into());
    }
    out.push("END:VEVENT".into());
}

/// A whole calendar: CRLF-joined, folded, ready to serve.
pub fn build_calendar(events: &[Event], name: &str, stamp: &str) -> String {
    let mut lines: Vec<String> = vec![
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        format!("PRODID:{PRODID}"),
        "CALSCALE:GREGORIAN".into(),
        "METHOD:PUBLISH".into(),
        format!("X-WR-CALNAME:{}", escape_text(name)),
        "X-PUBLISHED-TTL:PT12H".into(),
    ];
    for e in events {
        event_lines(e, stamp, &mut lines);
    }
    lines.push("END:VCALENDAR".into());
    let folded: Vec<String> = lines.iter().map(|l| fold_line(l)).collect();
    format!("{}\r\n", folded.join("\r\n"))
}

/// What a contract's events need besides the rules' fields.
#[derive(Debug, Clone, Default)]
pub struct EventContext {
    pub id: String,
    pub title: String,
    pub counterparty: String,
    pub updated_at_ms: i64,
    /// Link back to the contract (empty = none).
    pub url: String,
}

/// An obligation with what its events say.
#[derive(Debug, Clone)]
pub struct DatedObligation {
    pub id: String,
    pub text: String,
    pub owner: Owner,
    pub schedule: Obligation,
}

/// Event options.
#[derive(Debug, Clone)]
pub struct EventOptions {
    pub today: NaiveDate,
    /// Term ends to show on an auto-renewing contract.
    pub terms: usize,
    /// Occurrences to show of a repeating obligation.
    pub occurrences: usize,
    pub alarms: Vec<i64>,
}

fn context_lines(c: &Contract, x: &EventContext) -> Vec<String> {
    let mut v = vec![];
    if !x.counterparty.is_empty() {
        v.push(format!("Counterparty: {}", x.counterparty));
    }
    if let Some(val) = c.value {
        let money = if c.currency == "INR" {
            format!("₹{}", format_inr(val))
        } else {
            format!("{} {}", c.currency, format_inr(val))
        };
        v.push(format!("Value: {money} {}", c.billing));
    }
    if c.notice_days > 0 {
        v.push(format!("Notice period: {} days", c.notice_days));
    }
    v
}

/// Every dated deadline from `today` on: the notice deadline, this term's end and a bounded
/// number of renewals, and each open dated obligation with the next few of a repeating one.
/// Overdue obligations stay (they still need doing); nothing else from the past does.
pub fn contract_events(
    c: &Contract,
    x: &EventContext,
    obligations: &[DatedObligation],
    o: &EventOptions,
) -> Vec<Event> {
    if c.status != "active" {
        return vec![];
    }
    let today = o.today;
    let mut events = vec![];
    let sequence = sequence_for(x.updated_at_ms);
    let info = renewal_info(c, today, super::rules::DEFAULT_SOON_DAYS);
    let context = context_lines(c, x);
    let ev = |key: Vec<String>, date, summary: String, lead: String, cat: &str| Event {
        key,
        date,
        sequence,
        summary,
        description: std::iter::once(lead)
            .chain(context.iter().cloned())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        url: x.url.clone(),
        categories: vec![cat.to_owned()],
        alarms: o.alarms.clone(),
    };

    if let (Some(date), Some(nd)) = (info.date, info.notice_date) {
        if !info.handled && nd >= today {
            events.push(ev(
                vec![x.id.clone(), "notice".into(), nd.to_string()],
                nd,
                format!("Cancel or renegotiate {} by {}", x.title, format_date(nd)),
                format!(
                    "The term {} {}, so notice is due {} days earlier.",
                    if c.auto_renew { "renews" } else { "ends" },
                    format_date(date),
                    c.notice_days
                ),
                "Contract notice",
            ));
        }
    }

    if let Some(first) = info.date.filter(|d| *d >= today) {
        let decision = c.decision.as_ref();
        let ending = decision.is_some_and(|d| d.kind.ending() && d.for_term_end == Some(first));
        let count = if c.auto_renew && !ending {
            o.terms.max(1)
        } else {
            1
        };
        let months = c.term_months.max(1);
        let mut dates = vec![first];
        if let Some(end) = c.end_date {
            let mut j = 1;
            while dates.len() < count && j <= 1200 {
                let next = add_months(end, months * j);
                if next > first {
                    dates.push(next);
                }
                j += 1;
            }
        }
        for date in dates {
            let (summary, lead) = if ending {
                let why = if decision.is_some_and(|d| d.kind == DecisionKind::NoticeGiven) {
                    "Notice was given, so the term ends here."
                } else {
                    "Renewal was declined, so the term ends here."
                };
                (format!("{} ends", x.title), why.to_owned())
            } else if c.auto_renew {
                (
                    format!("{} renews", x.title),
                    format!("Auto-renews for another {months} months unless cancelled."),
                )
            } else {
                (
                    format!("{} ends", x.title),
                    "Fixed term, ends on this date.".to_owned(),
                )
            };
            let cat = if c.auto_renew && !ending {
                "Contract renewal"
            } else {
                "Contract end"
            };
            events.push(ev(
                vec![x.id.clone(), "term".into(), date.to_string()],
                date,
                summary,
                lead,
                cat,
            ));
        }
    }

    for ob in obligations {
        let s = &ob.schedule;
        let Some(due) = s.due.filter(|_| !s.done) else {
            continue;
        };
        let mut dates = vec![due];
        if let Some(r) = s.repeat.as_ref().filter(|r| repeat_months(&r.every) > 0) {
            let start = s.history.len() as i32;
            for i in 1..o.occurrences.max(1) as i32 {
                let index = start + i;
                if !within_series(r, index, Some(c), today) {
                    break;
                }
                dates.push(occurrence_due(r, index));
            }
        }
        let owed = match ob.owner {
            Owner::Them => "the counterparty",
            Owner::Mutual => "both parties",
            Owner::Us => "us",
        };
        let repeats = s
            .repeat
            .as_ref()
            .map(|r| format!("\nRepeats {}.", r.every))
            .unwrap_or_default();
        for date in dates {
            if days_between(today, date) < 0 && date != due {
                continue;
            }
            events.push(ev(
                vec![x.id.clone(), "ob".into(), ob.id.clone(), date.to_string()],
                date,
                format!("{} ({})", ob.text, x.title),
                format!("Owed by: {owed}{repeats}"),
                "Contract obligation",
            ));
        }
    }
    events
}

/// Events of many contracts in date order (then by UID).
pub fn sort_events(events: &mut [Event]) {
    events.sort_by(|a, b| a.date.cmp(&b.date).then_with(|| a.uid().cmp(&b.uid())));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_on_octets() {
        let line = "é".repeat(50);
        let f = fold_line(&line);
        assert!(f.split("\r\n").all(|p| p.len() <= 75));
        assert_eq!(f.replace("\r\n ", ""), line);
        assert_eq!(escape_text("a,b;c\\d\r\ne"), "a\\,b\\;c\\\\d\\ne");
    }
}
