//! Golden vectors for `money::allocate`, generated from the original Everything implementation
//! by `tools/golden/generate.mjs` (see `tools/golden/README.md`).

use rust_decimal::Decimal;
use sutra_kernel::money::{allocate, CurrencyCode, Money};

#[derive(serde::Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(serde::Deserialize)]
struct Case {
    input: Input,
    expected: Vec<i64>,
}

#[derive(serde::Deserialize)]
struct Input {
    total: i64,
    weights: Vec<i64>,
}

#[test]
fn allocate_matches_everything_golden_vectors() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("golden/money_allocate.json")).expect("valid fixture");
    assert!(fixture.cases.len() > 300, "fixture unexpectedly small");
    for (i, case) in fixture.cases.iter().enumerate() {
        let total = Money::new(Decimal::new(case.input.total, 2), CurrencyCode::INR);
        let weights: Vec<Decimal> = case
            .input
            .weights
            .iter()
            .map(|w| Decimal::from(*w))
            .collect();
        let got: Vec<i64> = allocate(total, &weights, 2)
            .expect("allocates")
            .iter()
            .map(|m| {
                let mut a = m.amount;
                a.rescale(2);
                i64::try_from(a.mantissa()).expect("fits")
            })
            .collect();
        assert_eq!(
            got, case.expected,
            "case {i}: {} / {:?}",
            case.input.total, case.input.weights
        );
        if !got.is_empty() {
            assert_eq!(
                got.iter().sum::<i64>(),
                case.input.total,
                "case {i} must sum to total"
            );
        }
    }
}
