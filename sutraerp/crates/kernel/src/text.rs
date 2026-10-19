//! Server-side text, contact and date policy shared by every module (audit remediation 2026-10,
//! slice A2: SEC-1 / C-1, VAL-1, server side of VAL-2/3/4 and H-2).
//!
//! The web client renders every value as text (React escapes it), so a stored `<img onerror>` is
//! shown literally rather than executed. This module is the second layer: names and codes that
//! identify a person, institution or course are refused at the API when they carry markup or
//! control characters, so they can never reach a less careful sink later (an e-mail template, a
//! PDF, a spreadsheet, a third-party integration). Every rule returns a field-level
//! [`FieldViolation`], so callers collect them into the usual `validation-failed` problem
//! (`validationErrors[]` with the camelCase field name).
//!
//! * identity fields (person / institution / course / staff / enquiry names, codes, titles):
//!   trimmed, required where the caller says so, at most `max` characters, single line, no control
//!   or bidi-override characters, and none of [`MARKUP_CHARS`].
//! * free text (notes, descriptions): at most `max` characters, no control characters other than
//!   line breaks and tabs.
//! * Indian mobile numbers: 10 digits starting 6–9, optionally prefixed `+91` / `91` / `0`;
//!   spaces, hyphens, dots and parentheses between digits are tolerated.
//! * e-mail addresses: `local@domain.tld`, no whitespace, ≤ 254 characters, a dotted domain of
//!   letters / digits / hyphens with an alphabetic TLD.
//! * dates of birth: required where the caller says so, not in the future, age within a range.

use crate::error::FieldViolation;
use chrono::{Datelike, NaiveDate};

/// Characters refused in identity fields: they open or close HTML / XML markup.
pub const MARKUP_CHARS: [char; 2] = ['<', '>'];

/// Youngest applicant age (years) accepted for a student admission.
pub const MIN_APPLICANT_AGE: u32 = 3;
/// Oldest applicant age (years) accepted for a student admission.
pub const MAX_APPLICANT_AGE: u32 = 100;

/// Unicode bidi embedding / override / isolate controls (used to disguise text direction).
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
}

/// A control character a single-line identity value may not contain (includes CR / LF / TAB).
fn is_single_line_forbidden(c: char) -> bool {
    c.is_control() || is_bidi_control(c) || c == '\u{2028}' || c == '\u{2029}'
}

/// A control character free text may not contain (line breaks and tabs are allowed).
fn is_free_text_forbidden(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || is_bidi_control(c)
}

/// Character-policy violation of an identity value (markup or control characters), if any.
/// Length and presence are the caller's (or [`identity`]'s) concern.
pub fn identity_chars(field: &str, raw: &str) -> Option<FieldViolation> {
    if raw.chars().any(is_single_line_forbidden) {
        Some(FieldViolation::new(
            field,
            "control-characters",
            format!("{field} must be a single line without control characters"),
        ))
    } else if raw.contains(MARKUP_CHARS) {
        Some(FieldViolation::new(
            field,
            "markup",
            format!("{field} must not contain < or >"),
        ))
    } else {
        None
    }
}

/// Validates an identity value (name, code, title). Pushes at most one violation and returns
/// the trimmed value (also when it is invalid, so callers can keep collecting violations).
pub fn identity(
    field: &str,
    raw: &str,
    max: usize,
    required: bool,
    v: &mut Vec<FieldViolation>,
) -> String {
    let t = raw.trim();
    let n = t.chars().count();
    if n == 0 {
        if required {
            v.push(FieldViolation::new(
                field,
                "required",
                format!("{field} is required"),
            ));
        }
    } else if n > max {
        v.push(FieldViolation::new(
            field,
            "too-long",
            format!("{field} must be at most {max} characters"),
        ));
    } else if let Some(x) = identity_chars(field, t) {
        v.push(x);
    }
    t.to_owned()
}

/// [`identity`] for an optional member: `None` / blank → `None`.
pub fn opt_identity(
    field: &str,
    raw: Option<&str>,
    max: usize,
    v: &mut Vec<FieldViolation>,
) -> Option<String> {
    raw.map(|r| identity(field, r, max, false, v))
        .filter(|s| !s.is_empty())
}

/// Control-character violation of free text (notes, descriptions), if any.
pub fn free_text_chars(field: &str, raw: &str) -> Option<FieldViolation> {
    raw.chars().any(is_free_text_forbidden).then(|| {
        FieldViolation::new(
            field,
            "control-characters",
            format!("{field} must not contain control characters"),
        )
    })
}

/// The ten national digits of an Indian mobile number, or `None` when `raw` is not one.
/// Accepts an optional `+91`, `91` or `0` prefix and spaces / hyphens / dots / parentheses.
pub fn indian_mobile(raw: &str) -> Option<String> {
    let t = raw.trim();
    let (plus, rest) = match t.strip_prefix('+') {
        Some(r) => (true, r),
        None => (false, t),
    };
    if !rest
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, ' ' | '-' | '.' | '(' | ')'))
    {
        return None;
    }
    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
    let national = match (plus, digits.len()) {
        (true, 12) => digits.strip_prefix("91")?,
        (true, _) => return None,
        (false, 10) => digits.as_str(),
        (false, 11) => digits.strip_prefix('0')?,
        (false, 12) => digits.strip_prefix("91")?,
        _ => return None,
    };
    (national.len() == 10 && national.starts_with(['6', '7', '8', '9']))
        .then(|| national.to_owned())
}

/// Validates an optional Indian mobile number (blank = absent).
pub fn check_indian_mobile(field: &str, raw: Option<&str>, v: &mut Vec<FieldViolation>) {
    if let Some(p) = raw.map(str::trim).filter(|p| !p.is_empty()) {
        if indian_mobile(p).is_none() {
            v.push(FieldViolation::new(
                field,
                "format",
                format!(
                    "{field} must be a 10-digit Indian mobile number starting 6-9 (optionally +91)"
                ),
            ));
        }
    }
}

/// True when `raw` (already trimmed) is a plausible e-mail address.
pub fn email_ok(e: &str) -> bool {
    if e.len() > 254 || e.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((local, domain)) = e.rsplit_once('@') else {
        return false;
    };
    if local.is_empty()
        || local.len() > 64
        || local.contains(['@', '<', '>', '(', ')', ',', ';', ':', '\\', '"', '[', ']'])
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let label_ok = |l: &&str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    };
    let tld = labels[labels.len() - 1];
    labels.iter().all(label_ok) && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

/// Validates an optional e-mail address (blank = absent).
pub fn check_email(field: &str, raw: Option<&str>, v: &mut Vec<FieldViolation>) {
    if let Some(e) = raw.map(str::trim).filter(|e| !e.is_empty()) {
        if !email_ok(e) {
            v.push(FieldViolation::new(
                field,
                "format",
                format!("{field} must be an e-mail address such as name@example.com"),
            ));
        }
    }
}

/// Whole years between `born` and `on` (`on` ≥ `born`).
pub fn age_on(born: NaiveDate, on: NaiveDate) -> u32 {
    let mut years = on.year() - born.year();
    if (on.month(), on.day()) < (born.month(), born.day()) {
        years -= 1;
    }
    u32::try_from(years.max(0)).unwrap_or(0)
}

/// Validates a date of birth against `today`: required when `required`, not in the future, and
/// an age of `min_age..=max_age` whole years.
pub fn check_birth_date(
    field: &str,
    dob: Option<NaiveDate>,
    today: NaiveDate,
    required: bool,
    (min_age, max_age): (u32, u32),
    v: &mut Vec<FieldViolation>,
) {
    match dob {
        None if required => v.push(FieldViolation::new(
            field,
            "required",
            format!("{field} is required"),
        )),
        None => {}
        Some(d) if d > today => v.push(FieldViolation::new(
            field,
            "future-date",
            format!("{field} cannot be in the future"),
        )),
        Some(d) => {
            let age = age_on(d, today);
            if !(min_age..=max_age).contains(&age) {
                v.push(FieldViolation::new(
                    field,
                    "out-of-range",
                    format!("{field} must give an age between {min_age} and {max_age} years"),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn identity_refuses_markup_and_controls() {
        let mut v = vec![];
        assert_eq!(
            identity("name", "  Sutra Institute  ", 200, true, &mut v),
            "Sutra Institute"
        );
        assert!(v.is_empty());
        identity("name", "<img src=x onerror=alert(1)>", 200, true, &mut v);
        identity("name", "a\u{0}b", 200, true, &mut v);
        identity("name", "line\nbreak", 200, true, &mut v);
        identity("name", "evil\u{202E}txt", 200, true, &mut v);
        identity("name", "   ", 200, true, &mut v);
        identity("name", &"x".repeat(201), 200, true, &mut v);
        let codes: Vec<&str> = v.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(
            codes,
            [
                "markup",
                "control-characters",
                "control-characters",
                "control-characters",
                "required",
                "too-long"
            ]
        );
        // Ordinary punctuation of real names stays allowed.
        let mut ok = vec![];
        identity(
            "name",
            "D'Souza & Sons, St. Mary's (Pune) — ज्ञान",
            200,
            true,
            &mut ok,
        );
        assert!(ok.is_empty(), "{ok:?}");
        assert_eq!(opt_identity("f", Some("  "), 10, &mut ok), None);
    }

    #[test]
    fn free_text_keeps_line_breaks() {
        assert!(free_text_chars("notes", "a\nb\tc").is_none());
        assert!(free_text_chars("notes", "a\u{7}b").is_some());
    }

    #[test]
    fn indian_mobiles() {
        for ok in [
            "9876543210",
            "+91 98765 43210",
            "+919876543210",
            "98200-12345",
            "091 98765 43210".trim_start_matches('0'),
            "09876543210",
            "6000000000",
        ] {
            assert!(indian_mobile(ok).is_some(), "{ok}");
        }
        for bad in [
            "123",
            "abc",
            "5876543210",
            "+1 415 555 0100",
            "98765432101",
            "",
            "+91 5876543210",
            "9876x43210",
        ] {
            assert!(indian_mobile(bad).is_none(), "{bad}");
        }
        assert_eq!(
            indian_mobile("+91 98765 43210").as_deref(),
            Some("9876543210")
        );
    }

    #[test]
    fn emails() {
        for ok in [
            "a@b.in",
            "first.last+tag@mail.example.org",
            "ANIKA.RAO@b7a.dev",
        ] {
            assert!(email_ok(ok), "{ok}");
        }
        for bad in [
            "abc",
            "a@b",
            "a@b.",
            "@b.in",
            "a b@c.in",
            "a@-b.in",
            "a@b.c",
            "a@b.12",
            "a..b@c.in",
            "<x>@y.in",
            "a@b_c.in",
        ] {
            assert!(!email_ok(bad), "{bad}");
        }
    }

    #[test]
    fn birth_dates() {
        let today = d("2026-10-19");
        let mut v = vec![];
        check_birth_date("dob", Some(d("2007-03-14")), today, true, (3, 100), &mut v);
        check_birth_date("dob", None, today, false, (3, 100), &mut v);
        assert!(v.is_empty());
        check_birth_date("dob", None, today, true, (3, 100), &mut v);
        check_birth_date("dob", Some(d("2026-10-20")), today, true, (3, 100), &mut v);
        check_birth_date("dob", Some(d("2024-01-01")), today, true, (3, 100), &mut v);
        check_birth_date("dob", Some(d("1900-01-01")), today, true, (3, 100), &mut v);
        let codes: Vec<&str> = v.iter().map(|x| x.code.as_str()).collect();
        assert_eq!(
            codes,
            ["required", "future-date", "out-of-range", "out-of-range"]
        );
        assert_eq!(age_on(d("2008-10-20"), today), 17);
        assert_eq!(age_on(d("2008-10-19"), today), 18);
    }
}
