//! Money helpers (ADR-L5-0032, contract §3.1): `rust_decimal::Decimal` end to end, stored as
//! NUMERIC(18,2) with a currency (INR in Batch 5). Never floats. On the wire amounts are JSON
//! strings (`"1250.00"`): DTO fields use `#[serde(with = "rust_decimal::serde::str")]` and
//! `#[schema(value_type = String, example = "1250.00")]`.

use rust_decimal::{Decimal, RoundingStrategy};
use sutra_contracts::finance::ports::{is_valid_positive_amount, MONEY_SCALE};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::money::CurrencyCode;

/// The Batch 5 currency.
pub const INR: CurrencyCode = CurrencyCode::INR;

/// Validates a positive amount (scale ≤ 2, fits NUMERIC(18,2)) → 422 on `field` otherwise.
pub fn require_positive(field: &str, amount: Decimal) -> Result<Decimal, KernelError> {
    if is_valid_positive_amount(amount) {
        Ok(amount.round_dp(MONEY_SCALE))
    } else {
        Err(KernelError::Validation(vec![FieldViolation::new(
            field,
            "invalid_amount",
            "must be a positive amount with at most two decimals",
        )]))
    }
}

/// Largest amount one payment / receipt / refund may carry: ₹1 crore (audit 2026-10 A2, VAL-2 /
/// H-2). A typo such as `99999999999` is refused instead of being receipted. Raise it here if an
/// institution genuinely takes larger single transfers.
pub const MAX_TRANSACTION_AMOUNT: Decimal = Decimal::from_parts(1_000_000_000, 0, 0, false, 2);

/// 422 on `field` when `amount` exceeds [`MAX_TRANSACTION_AMOUNT`].
pub fn require_within_transaction_cap(
    field: &str,
    amount: Decimal,
) -> Result<Decimal, KernelError> {
    if amount > MAX_TRANSACTION_AMOUNT {
        Err(KernelError::Validation(vec![FieldViolation::new(
            field,
            "amount_too_large",
            format!(
                "must be at most {} per transaction",
                display_inr(MAX_TRANSACTION_AMOUNT)
            ),
        )]))
    } else {
        Ok(amount)
    }
}

/// Validates a currency (Batch 5 accepts only INR) → 422 on `field` otherwise.
pub fn require_inr(field: &str, currency: &str) -> Result<CurrencyCode, KernelError> {
    if currency == INR.as_str() {
        Ok(INR)
    } else {
        Err(KernelError::Validation(vec![FieldViolation::new(
            field,
            "unsupported_currency",
            "only INR is supported",
        )]))
    }
}

/// Rounds a computed amount (percentages, pro-rata) once, to paise, half-up — the default
/// rounding rule until 5A's finance rules (F-085) make it configurable.
pub fn round_paise(amount: Decimal) -> Decimal {
    amount.round_dp_with_strategy(MONEY_SCALE, RoundingStrategy::MidpointAwayFromZero)
}

/// Canonical two-decimal string (`1250.00`).
pub fn fmt(amount: Decimal) -> String {
    let mut a = amount.round_dp(MONEY_SCALE);
    a.rescale(MONEY_SCALE);
    a.to_string()
}

/// Indian display form with the rupee sign and lakh grouping (`₹1,25,000.00`) for details shown
/// to staff / students (clearance evidence, notifications).
pub fn display_inr(amount: Decimal) -> String {
    let s = fmt(amount.abs());
    let (int, frac) = s.split_once('.').unwrap_or((s.as_str(), "00"));
    let digits: Vec<char> = int.chars().collect();
    let mut out = String::new();
    let n = digits.len();
    for (i, c) in digits.iter().enumerate() {
        out.push(*c);
        let rest = n - i - 1;
        if rest > 0 && (rest == 3 || (rest > 3 && (rest - 3).is_multiple_of(2))) {
            out.push(',');
        }
    }
    let sign = if amount.is_sign_negative() && !amount.is_zero() {
        "-"
    } else {
        ""
    };
    format!("{sign}₹{out}.{frac}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn transaction_cap_is_one_crore() {
        assert_eq!(MAX_TRANSACTION_AMOUNT, d("10000000.00"));
        assert!(require_within_transaction_cap("amount", d("10000000.00")).is_ok());
        assert!(require_within_transaction_cap("amount", d("10000000.01")).is_err());
        assert!(require_within_transaction_cap("amount", d("99999999999")).is_err());
    }

    #[test]
    fn amounts_validate_and_format() {
        assert_eq!(require_positive("amount", d("12.5")).ok(), Some(d("12.50")));
        assert!(require_positive("amount", d("0")).is_err());
        assert!(require_positive("amount", d("-1")).is_err());
        assert!(require_positive("amount", d("1.005")).is_err());
        assert!(require_inr("currency", "INR").is_ok());
        assert!(require_inr("currency", "USD").is_err());
        assert_eq!(fmt(d("12.5")), "12.50");
        assert_eq!(fmt(d("7")), "7.00");
        assert_eq!(round_paise(d("10.005")), d("10.01"));
        assert_eq!(round_paise(d("33.333333")), d("33.33"));
    }

    #[test]
    fn indian_grouping() {
        assert_eq!(display_inr(d("125000")), "₹1,25,000.00");
        assert_eq!(display_inr(d("12345678.9")), "₹1,23,45,678.90");
        assert_eq!(display_inr(d("999")), "₹999.00");
        assert_eq!(display_inr(d("1000")), "₹1,000.00");
        assert_eq!(display_inr(d("-50")), "-₹50.00");
    }
}
