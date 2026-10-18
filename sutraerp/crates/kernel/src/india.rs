//! Indian statutory identifier formats shared by every module that reads or prints them: the
//! GSTIN (with its base-36 check character) and the GST state codes. Pure, no business rules.
//!
//! Moved here from `sutra-resources` (`expenses::receipt_text::gstin_valid`, ERP_EXTENSIONS 3D)
//! when Finance started issuing GST tax invoices (ERP_EXTENSIONS 5D), so both use one validator.

const GSTIN_CHARS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// GSTIN checksum (base-36, weights 1/2 alternating): the 15th character.
pub fn gstin_checksum_valid(g: &str) -> bool {
    let g = g.as_bytes();
    if g.len() != 15 {
        return false;
    }
    let mut sum = 0u32;
    for (i, c) in g[..14].iter().enumerate() {
        let Some(v) = GSTIN_CHARS.find(*c as char) else {
            return false;
        };
        let p = v as u32 * if i % 2 == 0 { 1 } else { 2 };
        sum += p / 36 + p % 36;
    }
    let check = (36 - sum % 36) % 36;
    GSTIN_CHARS.as_bytes()[check as usize] == g[14]
}

/// A full GSTIN check: 15 characters, a known state code, a PAN (`AAAAA9999A`), the entity
/// number, `Z`, and the check character.
pub fn gstin_valid(g: &str) -> bool {
    let b = g.as_bytes();
    b.len() == 15
        && g.is_ascii()
        && state_name(&g[0..2]).is_some()
        && b[2..7].iter().all(u8::is_ascii_uppercase)
        && b[7..11].iter().all(u8::is_ascii_digit)
        && b[11].is_ascii_uppercase()
        && (b[12].is_ascii_uppercase() || (b'1'..=b'9').contains(&b[12]))
        && b[13] == b'Z'
        && gstin_checksum_valid(g)
}

/// The state code a GSTIN is registered in (its first two digits).
pub fn gstin_state(g: &str) -> Option<&str> {
    g.get(0..2).filter(|c| state_name(c).is_some())
}

/// GST state / union-territory codes (CBIC list; `97` other territory, `96` foreign country).
pub const GST_STATES: [(&str, &str); 39] = [
    ("01", "Jammu and Kashmir"),
    ("02", "Himachal Pradesh"),
    ("03", "Punjab"),
    ("04", "Chandigarh"),
    ("05", "Uttarakhand"),
    ("06", "Haryana"),
    ("07", "Delhi"),
    ("08", "Rajasthan"),
    ("09", "Uttar Pradesh"),
    ("10", "Bihar"),
    ("11", "Sikkim"),
    ("12", "Arunachal Pradesh"),
    ("13", "Nagaland"),
    ("14", "Manipur"),
    ("15", "Mizoram"),
    ("16", "Tripura"),
    ("17", "Meghalaya"),
    ("18", "Assam"),
    ("19", "West Bengal"),
    ("20", "Jharkhand"),
    ("21", "Odisha"),
    ("22", "Chhattisgarh"),
    ("23", "Madhya Pradesh"),
    ("24", "Gujarat"),
    ("26", "Dadra and Nagar Haveli and Daman and Diu"),
    ("27", "Maharashtra"),
    ("29", "Karnataka"),
    ("30", "Goa"),
    ("31", "Lakshadweep"),
    ("32", "Kerala"),
    ("33", "Tamil Nadu"),
    ("34", "Puducherry"),
    ("35", "Andaman and Nicobar Islands"),
    ("36", "Telangana"),
    ("37", "Andhra Pradesh"),
    ("38", "Ladakh"),
    ("96", "Foreign country"),
    ("97", "Other territory"),
    ("99", "Centre jurisdiction"),
];

/// Name of a GST state code (`27` → Maharashtra).
pub fn state_name(code: &str) -> Option<&'static str> {
    GST_STATES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, name)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gstin_checksum_and_format() {
        assert!(gstin_checksum_valid("27AAPFU0939F1ZV"));
        assert!(!gstin_checksum_valid("27AAPFU0939F1ZX"));
        assert!(gstin_valid("27AAPFU0939F1ZV"));
        assert!(!gstin_valid("27AAPFU0939F1Z"));
        assert!(
            !gstin_valid("28AAPFU0939F1ZV"),
            "check character covers the state code"
        );
        assert!(
            !gstin_valid("25AAPFU0939F1ZV"),
            "25 is not a state code any more"
        );
        assert_eq!(gstin_state("27AAPFU0939F1ZV"), Some("27"));
        assert_eq!(state_name("27"), Some("Maharashtra"));
        assert_eq!(state_name("00"), None);
    }
}
