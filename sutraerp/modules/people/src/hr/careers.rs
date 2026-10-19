//! **ERP_EXTENSIONS 5A** — the public careers page, ported from HeyPuter/everything
//! `src/hiring/src/core/jobpage.js` (`escapeHtml`, `descriptionHtml`, `subdomainBase`; MIT, see
//! THIRD_PARTY_NOTICES.md). Everything publishes a static page; SutraERP serves the same safe
//! fields from an unauthenticated, read-only endpoint keyed by the institution's careers slug,
//! plus a spam-safe apply form (honeypot, size limits, one application per e-mail per opening).

pub use super::offers::escape_html;

fn is_bullet(line: &str) -> Option<&str> {
    let mut chars = line.char_indices();
    match chars.next() {
        Some((_, '-' | '*')) => {}
        _ => return None,
    }
    let rest = &line[1..];
    let trimmed = rest.trim_start();
    (trimmed.len() < rest.len()).then_some(trimmed)
}

/// `descriptionHtml`: escaped paragraphs and `-` / `*` bullet lists.
pub fn description_html(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut para: Vec<String> = Vec::new();
    let mut bullets: Vec<String> = Vec::new();
    fn flush_p(out: &mut Vec<String>, para: &mut Vec<String>) {
        if !para.is_empty() {
            let inner = para
                .iter()
                .map(|l| escape_html(l))
                .collect::<Vec<_>>()
                .join(" ");
            out.push(format!("<p>{inner}</p>"));
            para.clear();
        }
    }
    fn flush_b(out: &mut Vec<String>, bullets: &mut Vec<String>) {
        if !bullets.is_empty() {
            let inner: String = bullets
                .iter()
                .map(|b| format!("<li>{}</li>", escape_html(b)))
                .collect();
            out.push(format!("<ul>{inner}</ul>"));
            bullets.clear();
        }
    }
    for raw in text.split('\n') {
        let line = raw.trim();
        if line.is_empty() {
            flush_p(&mut out, &mut para);
            flush_b(&mut out, &mut bullets);
        } else if let Some(item) = is_bullet(line) {
            flush_p(&mut out, &mut para);
            bullets.push(item.to_owned());
        } else {
            flush_b(&mut out, &mut bullets);
            para.push(line.to_owned());
        }
    }
    flush_p(&mut out, &mut para);
    flush_b(&mut out, &mut bullets);
    out.join("\n")
}

/// `subdomainBase`: a URL slug from the institution name (`jobs` when nothing is left).
pub fn slug_base(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut s = String::new();
    let mut dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
            dash = false;
        } else if !dash {
            s.push('-');
            dash = true;
        }
    }
    let trimmed: String = s.trim_matches('-').chars().take(30).collect();
    if trimmed.is_empty() {
        "jobs".into()
    } else {
        trimmed
    }
}

/// A valid careers slug: 3–40 of `a-z0-9-`, not starting or ending with `-`.
pub fn is_valid_slug(s: &str) -> bool {
    (3..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// Size limits of the public apply form.
pub const MAX_NAME: usize = 100;
pub const MAX_EMAIL: usize = 254;
pub const MAX_PHONE: usize = 20;
pub const MAX_NOTE: usize = 2000;
pub const MAX_LINK: usize = 300;
/// The whole request body (bytes).
pub const MAX_BODY: usize = 16 * 1024;

/// What the apply form sends.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Application {
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub link: Option<String>,
    pub note: Option<String>,
    /// The honeypot: hidden from people, filled by bots.
    pub website: Option<String>,
}

/// A cleaned application ready to become a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanApplication {
    pub given_name: String,
    pub family_name: Option<String>,
    pub email: String,
    pub phone: Option<String>,
    pub notes: Option<String>,
}

/// The outcome of checking an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// A bot filled the honeypot: answer as if accepted, store nothing.
    Spam,
    Invalid(Vec<(&'static str, &'static str)>),
    Ok(CleanApplication),
}

fn opt(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

fn valid_email(e: &str) -> bool {
    let parts: Vec<&str> = e.split('@').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && !parts[1].starts_with('.')
        && !parts[1].ends_with('.')
        && !e.chars().any(char::is_whitespace)
}

/// Honeypot, required fields, sizes and shapes.
pub fn check_application(a: &Application) -> Checked {
    if a.website.as_deref().is_some_and(|w| !w.trim().is_empty()) {
        return Checked::Spam;
    }
    let mut bad = Vec::new();
    let name = a.name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME {
        bad.push(("name", "your name (up to 100 characters)"));
    }
    let email = a.email.trim().to_lowercase();
    if email.len() > MAX_EMAIL || !valid_email(&email) {
        bad.push(("email", "a valid e-mail address"));
    }
    let phone = opt(&a.phone);
    if let Some(p) = &phone {
        let digits = p.chars().filter(char::is_ascii_digit).count();
        let ok_chars = p
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')'));
        if p.chars().count() > MAX_PHONE || !ok_chars || !(10..=15).contains(&digits) {
            bad.push(("phone", "a phone number of 10 to 15 digits"));
        }
    }
    let link = opt(&a.link);
    if let Some(l) = &link {
        if l.chars().count() > MAX_LINK || !(l.starts_with("https://") || l.starts_with("http://"))
        {
            bad.push(("link", "an http(s) link up to 300 characters"));
        }
    }
    let note = opt(&a.note);
    if note
        .as_deref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE)
    {
        bad.push(("note", "a note up to 2000 characters"));
    }
    if !bad.is_empty() {
        return Checked::Invalid(bad);
    }
    let (given, family) = match name.rsplit_once(char::is_whitespace) {
        Some((g, f)) if !g.trim().is_empty() => (g.trim().to_owned(), Some(f.trim().to_owned())),
        _ => (name.to_owned(), None),
    };
    let notes = match (link, note) {
        (None, None) => None,
        (Some(l), None) => Some(format!("Link: {l}")),
        (None, Some(n)) => Some(n),
        (Some(l), Some(n)) => Some(format!("{n}\n\nLink: {l}")),
    };
    Checked::Ok(CleanApplication {
        given_name: given,
        family_name: family,
        email,
        phone,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn honeypot_and_limits() {
        let mut a = Application {
            name: "Priya Kulkarni".into(),
            email: "Priya@Example.in".into(),
            phone: Some("+91 98220 12345".into()),
            ..Application::default()
        };
        match check_application(&a) {
            Checked::Ok(c) => {
                assert_eq!(c.given_name, "Priya");
                assert_eq!(c.family_name.as_deref(), Some("Kulkarni"));
                assert_eq!(c.email, "priya@example.in");
            }
            other => panic!("{other:?}"),
        }
        a.website = Some("http://spam".into());
        assert_eq!(check_application(&a), Checked::Spam);
        a.website = None;
        a.email = "nope".into();
        a.note = Some("x".repeat(2001));
        assert!(matches!(check_application(&a), Checked::Invalid(v) if v.len() == 2));
    }

    #[test]
    fn slugs() {
        assert_eq!(
            slug_base("Ahilyanagar Arts & Science College"),
            "ahilyanagar-arts-science-colle"
        );
        assert_eq!(slug_base("---"), "jobs");
        assert!(is_valid_slug("ahilyanagar-college"));
        assert!(!is_valid_slug("-bad"));
    }
}
