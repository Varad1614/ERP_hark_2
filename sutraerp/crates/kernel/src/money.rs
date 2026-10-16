//! Monetary representation (ADR-L5-0032).
//!
//! `rust_decimal::Decimal` is the authoritative monetary scalar. No global precision, scale or
//! rounding rule is introduced here: each calculation binds its own authoritative policy, which is
//! why rounding is an explicit argument rather than an implicit default. Amounts serialize as
//! JSON strings to avoid binary floating point on the wire.

use rust_decimal::{Decimal, RoundingStrategy};
use std::fmt;

/// ISO 4217 alphabetic currency code (e.g. `INR`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schema(value_type = String, example = "INR")]
pub struct CurrencyCode([u8; 3]);

impl CurrencyCode {
    pub const INR: CurrencyCode = CurrencyCode(*b"INR");

    pub fn as_str(&self) -> &str {
        // Invariant: always three ASCII uppercase letters.
        std::str::from_utf8(&self.0).unwrap_or("XXX")
    }
}

impl TryFrom<String> for CurrencyCode {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        let b = s.as_bytes();
        if b.len() == 3 && b.iter().all(u8::is_ascii_uppercase) {
            Ok(Self([b[0], b[1], b[2]]))
        } else {
            Err(format!("invalid ISO 4217 currency code: {s}"))
        }
    }
}

impl From<CurrencyCode> for String {
    fn from(c: CurrencyCode) -> Self {
        c.as_str().to_owned()
    }
}

impl fmt::Display for CurrencyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An exact monetary amount in a specific currency.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct Money {
    #[serde(with = "rust_decimal::serde::str")]
    #[schema(value_type = String, example = "1250.50")]
    pub amount: Decimal,
    pub currency: CurrencyCode,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MoneyError {
    #[error("currency mismatch: {0} vs {1}")]
    CurrencyMismatch(CurrencyCode, CurrencyCode),
    #[error("arithmetic overflow")]
    Overflow,
}

impl Money {
    pub fn new(amount: Decimal, currency: CurrencyCode) -> Self {
        Self { amount, currency }
    }

    pub fn zero(currency: CurrencyCode) -> Self {
        Self {
            amount: Decimal::ZERO,
            currency,
        }
    }

    pub fn checked_add(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(&other)?;
        self.amount
            .checked_add(other.amount)
            .map(|amount| Money {
                amount,
                currency: self.currency,
            })
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(self, other: Money) -> Result<Money, MoneyError> {
        self.same_currency(&other)?;
        self.amount
            .checked_sub(other.amount)
            .map(|amount| Money {
                amount,
                currency: self.currency,
            })
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_mul(self, factor: Decimal) -> Result<Money, MoneyError> {
        self.amount
            .checked_mul(factor)
            .map(|amount| Money {
                amount,
                currency: self.currency,
            })
            .ok_or(MoneyError::Overflow)
    }

    /// Rounds to `scale` decimal places with the caller-supplied, policy-bound strategy.
    pub fn round(self, scale: u32, strategy: RoundingStrategy) -> Money {
        Money {
            amount: self.amount.round_dp_with_strategy(scale, strategy),
            currency: self.currency,
        }
    }

    /// Sums an iterator of amounts that must all share `currency`.
    pub fn sum<I: IntoIterator<Item = Money>>(
        currency: CurrencyCode,
        items: I,
    ) -> Result<Money, MoneyError> {
        items
            .into_iter()
            .try_fold(Money::zero(currency), Money::checked_add)
    }

    fn same_currency(&self, other: &Money) -> Result<(), MoneyError> {
        if self.currency == other.currency {
            Ok(())
        } else {
            Err(MoneyError::CurrencyMismatch(self.currency, other.currency))
        }
    }
}

/// Error raised by [`allocate`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AllocateError {
    #[error("amount {0} has more than {1} decimal places")]
    NotWholeMinorUnits(Decimal, u32),
    #[error("allocation arithmetic overflow")]
    Overflow,
}

/// Splits `total` across `weights` exactly, in minor units of `scale` decimal places
/// (2 for rupees and paise).
///
/// The shares always sum to `total`: every exact share is floored to a whole minor unit and the
/// leftover units go one at a time to the shares that lost the most to flooring, ties going to the
/// earlier share. Signed weights work as they are (a discount line takes a negative share). When
/// the weights sum to zero the whole amount goes to the first share. An empty weight list returns
/// no shares.
///
/// Ported from Everything's `accounting/src/lib/money.js` `allocate()` (MIT, HeyPuter/everything
/// @ 06802a1), reworked to exact integer arithmetic. Golden vectors: `tests/golden/`.
pub fn allocate(
    total: Money,
    weights: &[Decimal],
    scale: u32,
) -> Result<Vec<Money>, AllocateError> {
    let n = weights.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    let factor = Decimal::from(10_i64.checked_pow(scale).ok_or(AllocateError::Overflow)?);
    let to_minor = |d: Decimal| -> Result<i128, AllocateError> {
        let m = d.checked_mul(factor).ok_or(AllocateError::Overflow)?;
        if m.fract() != Decimal::ZERO {
            return Err(AllocateError::NotWholeMinorUnits(d, scale));
        }
        i128::try_from(m.trunc()).map_err(|_| AllocateError::Overflow)
    };
    let total_minor = to_minor(total.amount)?;

    // Bring every weight to a whole number on one common scale, so the shares are exact rationals.
    let w_scale = weights.iter().map(Decimal::scale).max().unwrap_or(0);
    let mut ints: Vec<i128> = weights
        .iter()
        .map(|w| {
            let mut w = *w;
            w.rescale(w_scale);
            w.mantissa()
        })
        .collect();
    let mut sum = ints
        .iter()
        .try_fold(0_i128, |a, b| a.checked_add(*b))
        .ok_or(AllocateError::Overflow)?;

    let from_minor = |m: i128| -> Result<Money, AllocateError> {
        let v = i64::try_from(m).map_err(|_| AllocateError::Overflow)?;
        Ok(Money::new(Decimal::new(v, scale), total.currency))
    };

    if sum == 0 {
        let mut out = vec![from_minor(0)?; n];
        if let Some(first) = out.first_mut() {
            *first = from_minor(total_minor)?;
        }
        return Ok(out);
    }
    // A negative weight sum gives the same shares as the negated weights over a positive sum.
    if sum < 0 {
        sum = -sum;
        for w in &mut ints {
            *w = -*w;
        }
    }

    let mut floors = Vec::with_capacity(n);
    let mut rems = Vec::with_capacity(n);
    for w in &ints {
        let num = total_minor.checked_mul(*w).ok_or(AllocateError::Overflow)?;
        floors.push(num.div_euclid(sum));
        rems.push(num.rem_euclid(sum));
    }
    let floored = floors
        .iter()
        .try_fold(0_i128, |a, b| a.checked_add(*b))
        .ok_or(AllocateError::Overflow)?;
    // Flooring loses less than one unit per share, so 0 <= rest < n.
    let rest = usize::try_from(total_minor - floored).map_err(|_| AllocateError::Overflow)?;

    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| rems.get(b).cmp(&rems.get(a)).then(a.cmp(&b)));
    for &i in order.iter().take(rest) {
        if let Some(f) = floors.get_mut(i) {
            *f += 1;
        }
    }
    floors.into_iter().map(from_minor).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn exact_base10_arithmetic() {
        let a = Money::new(d("0.10"), CurrencyCode::INR);
        let b = Money::new(d("0.20"), CurrencyCode::INR);
        assert_eq!(a.checked_add(b).map(|m| m.amount), Ok(d("0.30")));
    }

    #[test]
    fn currency_mismatch_is_rejected() {
        let usd = CurrencyCode::try_from("USD".to_string()).unwrap_or(CurrencyCode::INR);
        let r = Money::new(d("1"), CurrencyCode::INR).checked_add(Money::new(d("1"), usd));
        assert!(matches!(r, Err(MoneyError::CurrencyMismatch(_, _))));
    }

    fn inr(s: &str) -> Money {
        Money::new(d(s), CurrencyCode::INR)
    }

    fn shares(total: &str, weights: &[&str]) -> Vec<String> {
        let w: Vec<Decimal> = weights.iter().map(|s| d(s)).collect();
        allocate(inr(total), &w, 2)
            .expect("allocates")
            .into_iter()
            .map(|m| m.amount.to_string())
            .collect()
    }

    #[test]
    fn allocate_splits_evenly_remainder_first() {
        assert_eq!(shares("1.00", &["1", "1", "1"]), ["0.34", "0.33", "0.33"]);
    }

    #[test]
    fn allocate_keeps_discount_sign() {
        assert_eq!(
            shares("-0.99", &["-100", "10", "-9"]),
            ["-1.00", "0.10", "-0.09"]
        );
    }

    #[test]
    fn allocate_zero_weights_go_to_first() {
        assert_eq!(shares("5.00", &["0", "0"]), ["5.00", "0.00"]);
        assert!(shares("5.00", &[]).is_empty());
    }

    #[test]
    fn allocate_rejects_sub_paisa_total() {
        assert_eq!(
            allocate(inr("1.005"), &[d("1")], 2),
            Err(AllocateError::NotWholeMinorUnits(d("1.005"), 2))
        );
    }

    #[test]
    fn allocate_fractional_weights_instalments() {
        // 40 / 30 / 30 instalment plan of a 1,00,001 rupee fee.
        assert_eq!(
            shares("100001.00", &["0.4", "0.3", "0.3"]),
            ["40000.40", "30000.30", "30000.30"]
        );
    }

    #[test]
    fn rounding_is_explicit() {
        let m = Money::new(d("10.125"), CurrencyCode::INR);
        assert_eq!(
            m.round(2, RoundingStrategy::MidpointNearestEven).amount,
            d("10.12")
        );
        assert_eq!(
            m.round(2, RoundingStrategy::MidpointAwayFromZero).amount,
            d("10.13")
        );
    }

    #[test]
    fn serializes_amount_as_string() {
        let m = Money::new(d("1250.50"), CurrencyCode::INR);
        let json = serde_json::to_value(m).unwrap_or_default();
        assert_eq!(
            json,
            serde_json::json!({"amount": "1250.50", "currency": "INR"})
        );
        let back: Money = serde_json::from_value(json).expect("roundtrip");
        assert_eq!(back, m);
    }

    #[test]
    fn rejects_bad_currency() {
        assert!(CurrencyCode::try_from("inr".to_string()).is_err());
        assert!(CurrencyCode::try_from("RUPEE".to_string()).is_err());
    }

    #[test]
    fn sums_lines() {
        let lines = ["100.00", "250.50", "0.25"].map(|s| Money::new(d(s), CurrencyCode::INR));
        assert_eq!(
            Money::sum(CurrencyCode::INR, lines).map(|m| m.amount),
            Ok(d("350.75"))
        );
    }
}
