//! File validation of the document service (pure, unit tested): allow-listed types detected from
//! the content (magic bytes — never trusted from the extension or the declared `Content-Type`),
//! size limits, active-content refusal for PDFs and file-name sanitising.

use sutra_kernel::error::{FieldViolation, KernelError};

/// One accepted document type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType {
    pub mime: &'static str,
    pub extensions: &'static [&'static str],
}

pub const PDF: FileType = FileType {
    mime: "application/pdf",
    extensions: &["pdf"],
};
pub const PNG: FileType = FileType {
    mime: "image/png",
    extensions: &["png"],
};
pub const JPEG: FileType = FileType {
    mime: "image/jpeg",
    extensions: &["jpg", "jpeg"],
};

/// Every accepted type (admission and record documents: scans and PDFs).
pub const ALLOWED: [FileType; 3] = [PDF, PNG, JPEG];

/// The type the content really is, from its leading bytes.
pub fn detect(bytes: &[u8]) -> Option<FileType> {
    if bytes.starts_with(b"%PDF-") {
        Some(PDF)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(PNG)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(JPEG)
    } else {
        None
    }
}

/// PDF features that run code or carry hidden files; such a document is refused.
const PDF_ACTIVE: [&[u8]; 4] = [b"/JavaScript", b"/JS", b"/Launch", b"/EmbeddedFile"];

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Longest kept file name (characters).
pub const MAX_FILENAME: usize = 120;

/// Keeps the last path segment, maps anything outside `[A-Za-z0-9 ._()-]` to `_`, collapses
/// repeats, trims dots / spaces / underscores at the ends and caps the length (keeping the
/// extension). `None` when nothing usable is left.
pub fn sanitize_filename(raw: &str) -> Option<String> {
    let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let mut out = String::with_capacity(last.len());
    for c in last.chars() {
        let keep = c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '(' | ')' | '-');
        let c = if keep { c } else { '_' };
        if (c == '_' || c == ' ' || c == '.') && out.ends_with(c) {
            continue;
        }
        out.push(c);
    }
    let trimmed = out.trim_matches(['.', ' ', '_']);
    if trimmed.is_empty() {
        return None;
    }
    let mut name = trimmed.to_owned();
    if name.chars().count() > MAX_FILENAME {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && e.len() <= 8 => (s.to_owned(), format!(".{e}")),
            _ => (name.clone(), String::new()),
        };
        let keep = MAX_FILENAME.saturating_sub(ext.len());
        name = format!("{}{ext}", stem.chars().take(keep).collect::<String>());
    }
    Some(name)
}

/// Lower-case extension of a sanitised name.
pub fn extension(name: &str) -> Option<String> {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| !e.is_empty())
}

/// Media type without parameters, lower case.
fn media_type(ct: &str) -> String {
    ct.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// Validated upload: the detected type and the sanitised name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub file_type: FileType,
    pub filename: String,
}

fn violation(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Applies every rule; the first broken one is reported (422 problem with a field violation).
pub fn check(
    bytes: &[u8],
    filename: &str,
    declared_content_type: Option<&str>,
    max_bytes: u64,
) -> Result<Checked, KernelError> {
    if bytes.is_empty() {
        return Err(violation("file", "empty", "the file is empty"));
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        return Err(violation(
            "file",
            "too_large",
            format!("the file is larger than {max_bytes} bytes"),
        ));
    }
    let Some(file_type) = detect(bytes) else {
        return Err(violation(
            "file",
            "type_not_allowed",
            "only PDF, PNG and JPEG documents are accepted (checked from the file content)",
        ));
    };
    if let Some(ct) = declared_content_type.map(media_type) {
        if ct != file_type.mime && ct != "application/octet-stream" {
            return Err(violation(
                "contentType",
                "type_mismatch",
                format!(
                    "the declared type {ct} does not match the content ({})",
                    file_type.mime
                ),
            ));
        }
    }
    let Some(name) = sanitize_filename(filename) else {
        return Err(violation("filename", "invalid", "a file name is required"));
    };
    match extension(&name) {
        Some(e) if file_type.extensions.contains(&e.as_str()) => {}
        _ => {
            return Err(violation(
                "filename",
                "extension_mismatch",
                format!(
                    "the file name must end in .{} for {} content",
                    file_type.extensions.join(" / ."),
                    file_type.mime
                ),
            ))
        }
    }
    if file_type == PDF && PDF_ACTIVE.iter().any(|n| contains(bytes, n)) {
        return Err(violation(
            "file",
            "active_content",
            "PDF documents with scripts, launch actions or embedded files are not accepted",
        ));
    }
    Ok(Checked {
        file_type,
        filename: name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PDF_BYTES: &[u8] = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n%%EOF\n";
    const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    const JPEG_BYTES: &[u8] = b"\xff\xd8\xff\xe0\0\x10JFIF";

    fn code(r: Result<Checked, KernelError>) -> String {
        match r {
            Err(KernelError::Validation(v)) => format!("{}:{}", v[0].field, v[0].code),
            Err(e) => format!("other:{e}"),
            Ok(c) => format!("ok:{}:{}", c.file_type.mime, c.filename),
        }
    }

    #[test]
    fn types_come_from_magic_bytes() {
        assert_eq!(detect(PDF_BYTES), Some(PDF));
        assert_eq!(detect(PNG_BYTES), Some(PNG));
        assert_eq!(detect(JPEG_BYTES), Some(JPEG));
        assert_eq!(detect(b"MZ\x90\0 windows exe"), None);
        assert_eq!(detect(b"<html><script>"), None);
        assert_eq!(detect(b"PK\x03\x04 zip / docx"), None);
        assert_eq!(ALLOWED.len(), 3);
    }

    #[test]
    fn accepts_matching_files() {
        assert_eq!(
            code(check(
                PDF_BYTES,
                "Aadhaar card.pdf",
                Some("application/pdf"),
                1024
            )),
            "ok:application/pdf:Aadhaar card.pdf"
        );
        assert_eq!(
            code(check(
                JPEG_BYTES,
                "scan.JPEG",
                Some("image/jpeg; charset=x"),
                1024
            )),
            "ok:image/jpeg:scan.JPEG"
        );
        assert_eq!(
            code(check(
                PNG_BYTES,
                "a.png",
                Some("application/octet-stream"),
                1024
            )),
            "ok:image/png:a.png"
        );
        assert_eq!(
            code(check(PNG_BYTES, "a.png", None, 1024)),
            "ok:image/png:a.png"
        );
    }

    #[test]
    fn rejects_what_is_not_allowed() {
        assert_eq!(code(check(b"", "a.pdf", None, 10)), "file:empty");
        assert_eq!(code(check(PDF_BYTES, "a.pdf", None, 10)), "file:too_large");
        // An executable renamed to .pdf with a PDF content type is still refused.
        assert_eq!(
            code(check(
                b"MZ\x90\0",
                "invoice.pdf",
                Some("application/pdf"),
                1024
            )),
            "file:type_not_allowed"
        );
        assert_eq!(
            code(check(PDF_BYTES, "a.pdf", Some("image/png"), 1024)),
            "contentType:type_mismatch"
        );
        assert_eq!(
            code(check(PDF_BYTES, "a.png", None, 1024)),
            "filename:extension_mismatch"
        );
        assert_eq!(
            code(check(PDF_BYTES, "noext", None, 1024)),
            "filename:extension_mismatch"
        );
        assert_eq!(
            code(check(PDF_BYTES, "../..", None, 1024)),
            "filename:invalid"
        );
        assert_eq!(
            code(check(
                b"%PDF-1.4 << /OpenAction << /S /JavaScript /JS (app.alert(1)) >> >>",
                "a.pdf",
                None,
                1024
            )),
            "file:active_content"
        );
    }

    #[test]
    fn filenames_are_sanitised() {
        assert_eq!(
            sanitize_filename("../../etc/passwd.pdf").as_deref(),
            Some("passwd.pdf")
        );
        assert_eq!(
            sanitize_filename("C:\\Users\\x\\HSC marks.pdf").as_deref(),
            Some("HSC marks.pdf")
        );
        assert_eq!(
            sanitize_filename("a<script>.pdf").as_deref(),
            Some("a_script_.pdf")
        );
        assert_eq!(
            sanitize_filename("résumé\r\n.pdf").as_deref(),
            Some("r_sum_.pdf")
        );
        assert_eq!(sanitize_filename("...").as_deref(), None);
        assert_eq!(sanitize_filename("").as_deref(), None);
        let long = format!("{}.pdf", "x".repeat(400));
        let s = sanitize_filename(&long).unwrap_or_default();
        assert_eq!(s.chars().count(), MAX_FILENAME);
        assert!(s.ends_with(".pdf"));
        assert_eq!(extension("A.PDF").as_deref(), Some("pdf"));
    }
}
