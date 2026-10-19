//! **ERP_EXTENSIONS 5B** — clock punches on duty shifts, ported from HeyPuter/everything
//! `src/shifts/src/core/attendance.js` (MIT, see THIRD_PARTY_NOTICES.md): lateness, worked time
//! across midnight, clock-in / clock-out records, show-up summaries.
//!
//! One record per shift: `{ status: present | late | absent, clockIn?, clockOut?, note? }`. Pure:
//! the HTTP layer loads the records (`people.duty_shift_punch`) and asks these.

use super::rules::{add_days, duration_minutes, to_minutes, Shift};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `ATTENDANCE_STATUS`.
pub const PRESENT: &str = "present";
pub const LATE: &str = "late";
pub const ABSENT: &str = "absent";

/// `isValidAttendanceStatus`.
pub fn is_valid_status(status: &str) -> bool {
    matches!(status, PRESENT | LATE | ABSENT)
}

/// An attendance record of one shift. Absent fields stay absent (Everything spreads the record
/// it was given into the next one).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Record {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_in: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_out: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Record {
    fn status(&self) -> &str {
        self.status.as_deref().unwrap_or("")
    }
    fn clock_in(&self) -> &str {
        self.clock_in.as_deref().unwrap_or("")
    }
    fn clock_out(&self) -> &str {
        self.clock_out.as_deref().unwrap_or("")
    }
    /// Recorded as absent.
    pub fn is_absent(&self) -> bool {
        self.status() == ABSENT
    }
}

/// `lateMinutes`: minutes late against the shift start; 0 when on time or unknown. For an
/// overnight shift a clock-in after midnight (at or before the shift's end) is the next day.
pub fn late_minutes(start: &str, end: &str, clock_in: &str) -> i64 {
    let (Some(s), Some(at)) = (to_minutes(start), to_minutes(clock_in)) else {
        return 0;
    };
    if at >= s {
        return at - s;
    }
    if to_minutes(end).is_some_and(|e| e < s && at <= e) {
        return at + 1440 - s;
    }
    0
}

/// `workedMinutes`: minutes between clock-in and clock-out; `None` when incomplete or
/// zero-length. An earlier clock-out crosses midnight.
pub fn worked_minutes(clock_in: &str, clock_out: &str) -> Option<i64> {
    let (a, b) = (to_minutes(clock_in)?, to_minutes(clock_out)?);
    if a == b {
        None
    } else if b > a {
        Some(b - a)
    } else {
        Some(b + 1440 - a)
    }
}

/// `elapsedMinutes`: minutes from the clock-in to `now` (an earlier time is the next morning).
pub fn elapsed_minutes(clock_in: &str, now: &str) -> Option<i64> {
    let (a, n) = (to_minutes(clock_in)?, to_minutes(now)?);
    Some(if n >= a { n - a } else { n + 1440 - a })
}

/// `isInProgress`: clocked in and not out yet.
pub fn is_in_progress(r: Option<&Record>) -> bool {
    r.is_some_and(|r| !r.is_absent() && !r.clock_in().is_empty() && r.clock_out().is_empty())
}

/// `isRunningNow`: in progress and the shift is today's or yesterday's (a night shift).
pub fn is_running_now(shift_date: &str, r: Option<&Record>, today: &str) -> bool {
    is_in_progress(r) && (shift_date == today || shift_date == add_days(today, -1))
}

/// `clockInRecord`: present within the grace period (or early), late past it. A record that
/// already has a clock-in keeps its status, so a manual choice survives a time correction.
pub fn clock_in_record(shift: &Shift, r: Option<&Record>, at: &str, grace: f64) -> Record {
    let grace = if grace.is_finite() {
        grace.max(0.0)
    } else {
        0.0
    };
    let late = late_minutes(&shift.start, &shift.end, at) as f64 > grace;
    let kept = r
        .filter(|x| !x.clock_in().is_empty())
        .and_then(|x| x.status.clone());
    let base = r.cloned().unwrap_or_default();
    Record {
        note: base.note.clone().or_else(|| Some(String::new())),
        status: Some(kept.unwrap_or_else(|| if late { LATE } else { PRESENT }.to_owned())),
        clock_in: Some(at.to_owned()),
        clock_out: Some(base.clock_out.unwrap_or_default()),
    }
}

/// `clockOutRecord`.
pub fn clock_out_record(r: Option<&Record>, at: &str) -> Record {
    let base = r.cloned().unwrap_or_default();
    Record {
        status: Some(base.status.unwrap_or_else(|| PRESENT.to_owned())),
        clock_in: Some(base.clock_in.unwrap_or_default()),
        note: Some(base.note.unwrap_or_default()),
        clock_out: Some(at.to_owned()),
    }
}

/// `summarizeAttendance`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Summary {
    pub total: usize,
    pub present: usize,
    pub late: usize,
    pub absent: usize,
    /// Share of recorded shifts the person showed up for (present or late); null when empty.
    pub rate: Option<f64>,
}

/// `summarizeAttendance`: counts over records; `rate` is (present + late) / total.
pub fn summarize(records: &[Record]) -> Summary {
    let mut s = Summary {
        total: records.len(),
        ..Summary::default()
    };
    for r in records {
        match r.status() {
            PRESENT => s.present += 1,
            LATE => s.late += 1,
            ABSENT => s.absent += 1,
            _ => {}
        }
    }
    s.rate = (s.total > 0).then(|| (s.present + s.late) as f64 / s.total as f64);
    s
}

/// `attendanceForRange`: assigned shifts in `[from, to]` with their record (if any).
pub fn for_range<'a>(
    shifts: &'a [Shift],
    records: &'a BTreeMap<String, Record>,
    from: &str,
    to: &str,
) -> Vec<(&'a Shift, Option<&'a Record>)> {
    shifts
        .iter()
        .filter(|s| {
            s.assignee_id.as_deref().is_some_and(|a| !a.is_empty())
                && s.date.as_str() >= from
                && s.date.as_str() <= to
        })
        .map(|s| (s, records.get(&s.id)))
        .collect()
}

/// ERP: minutes left before the scheduled end (overnight aware); 0 when on time or later.
pub fn early_leave_minutes(start: &str, end: &str, clock_out: &str) -> i64 {
    let (Some(s), Some(out), Some(d)) = (
        to_minutes(start),
        to_minutes(clock_out),
        duration_minutes(start, end),
    ) else {
        return 0;
    };
    let rel = (out - s).rem_euclid(1440);
    if rel < d {
        d - rel
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lateness_worked_time_and_records() {
        assert_eq!(late_minutes("09:00", "17:00", "09:25"), 25);
        assert_eq!(late_minutes("09:00", "17:00", "08:45"), 0);
        assert_eq!(late_minutes("22:00", "06:00", "00:15"), 135);
        assert_eq!(late_minutes("22:00", "06:00", "21:00"), 0);
        assert_eq!(worked_minutes("22:00", "06:00"), Some(480));
        assert_eq!(worked_minutes("09:00", "09:00"), None);
        assert_eq!(early_leave_minutes("09:00", "17:00", "16:30"), 30);
        assert_eq!(early_leave_minutes("22:00", "06:00", "05:00"), 60);
        assert_eq!(early_leave_minutes("22:00", "06:00", "06:20"), 0);
        let shift = Shift {
            start: "09:00".into(),
            end: "17:00".into(),
            ..Shift::default()
        };
        let r = clock_in_record(&shift, None, "09:20", 10.0);
        assert_eq!(r.status.as_deref(), Some(LATE));
        let r = clock_out_record(Some(&r), "17:05");
        assert_eq!(r.clock_out.as_deref(), Some("17:05"));
        assert!(!is_in_progress(Some(&r)));
        let s = summarize(&[r, Record::default()]);
        assert_eq!((s.total, s.late), (2, 1));
    }
}
