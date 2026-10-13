//! Pure treasury rules: bank account transitions, the JSON-lines statement parser and the
//! statement ↔ ledger match score.

use crate::ledger::common::invalid;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use std::str::FromStr;
use sutra_contracts::finance::status::BankAccountStatus as B;
use sutra_kernel::error::KernelError;

/// Legal bank account transitions.
pub fn can_transition_account(from: B, to: B) -> bool {
    matches!(
        (from, to),
        (B::Requested, B::Approved)
            | (B::Requested, B::Closed)
            | (B::Approved, B::Active)
            | (B::Active, B::Suspended)
            | (B::Active, B::Closed)
            | (B::Suspended, B::Active)
            | (B::Suspended, B::Closed)
    )
}

/// One parsed statement line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatementLine {
    pub value_date: NaiveDate,
    /// `true` = money into the account.
    pub credit: bool,
    pub amount: Decimal,
    pub reference: Option<String>,
    pub description: Option<String>,
}

const MAX_LINES: usize = 5000;

/// Parses JSON lines (blank lines skipped): `{"valueDate"|"date", "direction": "credit|debit",
/// "amount": "decimal string"}` plus optional `reference` / `description`. Every line must fall
/// inside `[from, to]`. Errors are `422` on `content` naming the line.
pub fn parse_statement(
    content: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<StatementLine>, KernelError> {
    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let n = i + 1;
        let bad = |m: &str| invalid("content", "invalid_line", format!("line {n}: {m}"));
        let v: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| bad("not a JSON object"))?;
        let obj = v.as_object().ok_or_else(|| bad("not a JSON object"))?;
        let date_raw = obj
            .get("valueDate")
            .or_else(|| obj.get("date"))
            .and_then(|d| d.as_str())
            .ok_or_else(|| bad("valueDate is required"))?;
        let value_date = NaiveDate::parse_from_str(date_raw, "%Y-%m-%d")
            .map_err(|_| bad("valueDate must be YYYY-MM-DD"))?;
        if value_date < from || value_date > to {
            return Err(bad("valueDate outside the statement period"));
        }
        let credit = match obj.get("direction").and_then(|d| d.as_str()) {
            Some("credit") => true,
            Some("debit") => false,
            _ => return Err(bad("direction must be credit or debit")),
        };
        // Amounts are decimal strings (never JSON numbers → no float on the way in).
        let amount = obj
            .get("amount")
            .and_then(|a| a.as_str())
            .and_then(|a| Decimal::from_str(a.trim()).ok())
            .filter(|a| *a > Decimal::ZERO && a.scale() <= 2)
            .ok_or_else(|| bad("amount must be a positive decimal string with ≤ 2 decimals"))?;
        let text = |k: &str| {
            obj.get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.chars().take(500).collect::<String>())
        };
        out.push(StatementLine {
            value_date,
            credit,
            amount,
            reference: text("reference"),
            description: text("description"),
        });
        if out.len() > MAX_LINES {
            return Err(invalid(
                "content",
                "too_many",
                format!("at most {MAX_LINES} lines per statement"),
            ));
        }
    }
    if out.is_empty() {
        return Err(invalid("content", "empty", "the statement has no lines"));
    }
    Ok(out)
}

/// Match score of a candidate for a statement line (higher is better), `None` when it cannot
/// match: the amount must be equal and the dates at most 5 days apart; a reference contained in
/// the candidate's reference text wins over date proximity.
pub fn match_score(
    amount: Decimal,
    date: NaiveDate,
    reference: Option<&str>,
    cand_amount: Decimal,
    cand_date: NaiveDate,
    cand_reference: &str,
) -> Option<i64> {
    if amount != cand_amount {
        return None;
    }
    let days = (date - cand_date).num_days().abs();
    if days > 5 {
        return None;
    }
    let reference_hit = reference
        .map(str::trim)
        .filter(|r| r.len() >= 3)
        .is_some_and(|r| cand_reference.to_lowercase().contains(&r.to_lowercase()));
    Some(if reference_hit { 100 } else { 0 } + (10 - days))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    #[test]
    fn statement_lines_parse_strictly() {
        let c = r#"{"valueDate":"2026-07-03","direction":"credit","amount":"99000.00","reference":"UTR1"}

{"date":"2026-07-04","direction":"debit","amount":"12.5","description":"charges"}"#;
        let l = parse_statement(c, d(7, 1), d(7, 31)).unwrap();
        assert_eq!(l.len(), 2);
        assert!(l[0].credit);
        assert_eq!(l[0].reference.as_deref(), Some("UTR1"));
        assert_eq!(l[1].amount, Decimal::new(1250, 2));
        for bad in [
            r#"{"valueDate":"2026-07-03","direction":"credit","amount":99000}"#,
            r#"{"valueDate":"2026-08-03","direction":"credit","amount":"1.00"}"#,
            r#"{"valueDate":"2026-07-03","direction":"in","amount":"1.00"}"#,
            r#"{"valueDate":"2026-07-03","direction":"credit","amount":"1.001"}"#,
            "not json",
            "",
        ] {
            assert!(parse_statement(bad, d(7, 1), d(7, 31)).is_err(), "{bad}");
        }
    }

    #[test]
    fn match_scores_prefer_reference_then_date() {
        let a = Decimal::new(1000, 0);
        assert!(match_score(a, d(7, 3), None, Decimal::new(999, 0), d(7, 3), "").is_none());
        assert!(match_score(a, d(7, 3), None, a, d(7, 10), "").is_none());
        let near = match_score(a, d(7, 3), Some("UTR9"), a, d(7, 3), "JE-000001").unwrap();
        let far_ref = match_score(a, d(7, 3), Some("UTR9"), a, d(7, 7), "settlement utr9").unwrap();
        assert!(far_ref > near);
        assert!(can_transition_account(B::Requested, B::Approved));
        assert!(!can_transition_account(B::Closed, B::Active));
    }
}
