//! Decimal helpers (ADR-L5-0032): money, leave days, rates and hours are `rust_decimal` end to
//! end and **strings on the wire** (`"45250.00"`). Computed values are rounded once with
//! [`round2`] (half away from zero).

use rust_decimal::{Decimal, RoundingStrategy};
use std::str::FromStr;
use sutra_kernel::error::{FieldViolation, KernelError};

/// Rounds to two places, half away from zero.
pub fn round2(v: Decimal) -> Decimal {
    v.round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero)
}

/// Two-place wire string (`"12.50"`, `"-3.00"`).
pub fn fmt2(v: Decimal) -> String {
    let mut r = round2(v);
    r.rescale(2);
    r.to_string()
}

/// Parses a decimal string field (422 `invalid_number` otherwise); at most two decimals.
pub fn parse_decimal(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = Decimal::from_str(raw.trim()).map_err(|_| {
        KernelError::Validation(vec![FieldViolation::new(
            field,
            "invalid_number",
            "must be a decimal string such as \"12.50\"",
        )])
    })?;
    if v.normalize().scale() > 2 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            field,
            "too_precise",
            "at most two decimals",
        )]));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn rounding_and_formatting() {
        assert_eq!(round2(d("2.345")), d("2.35"));
        assert_eq!(round2(d("-2.345")), d("-2.35"));
        assert_eq!(fmt2(d("12.5")), "12.50");
        assert_eq!(fmt2(d("3")), "3.00");
        assert!(parse_decimal("days", "1.5").is_ok());
        assert!(parse_decimal("days", "1.555").is_err());
        assert!(parse_decimal("days", "x").is_err());
    }
}
