//! **ERP_EXTENSIONS 5B** — the duty roster as an iCalendar file, ported from
//! HeyPuter/everything `src/shifts/src/core/ics.js` (MIT, see THIRD_PARTY_NOTICES.md).
//!
//! One VEVENT per assigned shift, floating local times (shifts are wall-clock ranges in the
//! institution's time), UIDs from shift ids so a re-import replaces events, RFC 5545 text
//! escaping of every line break, and folding at 74 octets without splitting a character.

use super::rules::{add_days, is_overnight, num, Shift};
use serde::Deserialize;
use std::collections::HashMap;

/// A shift with the extras the calendar shows.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CalendarShift {
    #[serde(flatten)]
    pub shift: Shift,
    pub note: Option<String>,
}

/// RFC 5545 text escaping: backslash, semicolon, comma and every kind of line break.
pub fn escape_text(v: &str) -> String {
    let s = v
        .replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,");
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str("\\n");
            }
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out
}

/// Folds a content line at 74 octets, continuing with CRLF + space.
pub fn fold(line: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut octets = 0;
    for ch in line.chars() {
        let size = ch.len_utf8();
        if octets + size > 74 && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
            current.push(' ');
            octets = 1;
        }
        current.push(ch);
        octets += size;
    }
    parts.push(current);
    parts.join("\r\n")
}

fn dt_local(date: &str, hhmm: &str) -> String {
    format!("{}T{}00", date.replace('-', ""), hhmm.replacen(':', "", 1))
}

/// `dtStamp`: an ISO timestamp → `20260805T093000Z`.
pub fn dt_stamp(iso: &str) -> String {
    let head: String = iso.chars().take(19).collect();
    format!("{}Z", head.replace(['-', ':'], ""))
}

/// `buildScheduleICS`: assigned shifts in `[from, to]` (optionally one person's) and how many.
/// `names` maps staff ids to names (a missing one is a former employee).
pub fn build_schedule_ics(
    names: &HashMap<String, String>,
    shifts: &[CalendarShift],
    from: &str,
    to: &str,
    employee: Option<&str>,
    generated_at: &str,
    calendar_name: &str,
) -> (String, usize) {
    let mut list: Vec<&CalendarShift> = shifts
        .iter()
        .filter(|c| {
            let s = &c.shift;
            let who = s.assignee_id.as_deref().unwrap_or("");
            !who.is_empty()
                && s.date.as_str() >= from
                && s.date.as_str() <= to
                && employee.is_none_or(|e| e.is_empty() || e == who)
        })
        .collect();
    list.sort_by(|a, b| {
        a.shift
            .date
            .cmp(&b.shift.date)
            .then_with(|| a.shift.start.cmp(&b.shift.start))
    });
    let stamp = dt_stamp(generated_at);
    let mut lines: Vec<String> = vec![
        "BEGIN:VCALENDAR".into(),
        "VERSION:2.0".into(),
        "PRODID:-//Shifts//Schedule//EN".into(),
        "CALSCALE:GREGORIAN".into(),
        "METHOD:PUBLISH".into(),
        format!("X-WR-CALNAME:{}", escape_text(calendar_name)),
    ];
    for c in &list {
        let s = &c.shift;
        let who = s.assignee_id.as_deref().unwrap_or("");
        let name = names
            .get(who)
            .cloned()
            .unwrap_or_else(|| "Former employee".to_owned());
        let end_date = if is_overnight(&s.start, &s.end) {
            add_days(&s.date, 1)
        } else {
            s.date.clone()
        };
        let mut description: Vec<String> = Vec::new();
        if let Some(b) = s.break_minutes.filter(|b| *b > 0.0) {
            description.push(format!("{} min unpaid break", num(b)));
        }
        if let Some(n) = c.note.as_deref().filter(|n| !n.is_empty()) {
            description.push(n.to_owned());
        }
        let description = description.join("\n");
        lines.push("BEGIN:VEVENT".into());
        lines.push(format!("UID:{}@shifts.app", escape_text(&s.id)));
        lines.push(format!("DTSTAMP:{stamp}"));
        lines.push(format!("DTSTART:{}", dt_local(&s.date, &s.start)));
        lines.push(format!("DTEND:{}", dt_local(&end_date, &s.end)));
        let summary = if s.role.is_empty() {
            name
        } else {
            format!("{} ({name})", s.role)
        };
        lines.push(format!("SUMMARY:{}", escape_text(&summary)));
        if !description.is_empty() {
            lines.push(format!("DESCRIPTION:{}", escape_text(&description)));
        }
        lines.push("END:VEVENT".into());
    }
    lines.push("END:VCALENDAR".into());
    let text = lines
        .iter()
        .map(|l| fold(l))
        .collect::<Vec<_>>()
        .join("\r\n");
    (format!("{text}\r\n"), list.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_escape_and_fold() {
        let mut names = HashMap::new();
        names.insert("e1".to_owned(), "Ava Chen".to_owned());
        let shifts = vec![CalendarShift {
            shift: Shift {
                id: "s1".into(),
                date: "2026-08-03".into(),
                start: "22:00".into(),
                end: "06:00".into(),
                role: "Night".into(),
                assignee_id: Some("e1".into()),
                break_minutes: Some(30.0),
            },
            note: Some(format!("one\rEND:VEVENT\r\n{}", "🎉".repeat(40))),
        }];
        let (ics, n) = build_schedule_ics(
            &names,
            &shifts,
            "2026-08-03",
            "2026-08-09",
            None,
            "2026-08-01T10:00:00.000Z",
            "Shifts schedule",
        );
        assert_eq!(n, 1);
        assert!(ics.contains("DTEND:20260804T060000"));
        assert!(ics.contains("DTSTAMP:20260801T100000Z"));
        assert!(ics.contains("SUMMARY:Night (Ava Chen)"));
        assert_eq!(
            ics.split("\r\n").filter(|l| *l == "BEGIN:VEVENT").count(),
            1
        );
        assert!(ics.split("\r\n").all(|l| l.len() <= 75));
    }
}
