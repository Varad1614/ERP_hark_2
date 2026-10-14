//! Library state machines and pure rules (L2-G05; contract `docs/batch8-contracts.md` §4.2):
//! policy, membership, catalogue, copy, loan, reservation, fine and clearance statuses, their
//! legal moves (illegal → `409`) and the fine arithmetic (`rust_decimal`, never floats).

use chrono::NaiveDate;
use rust_decimal::Decimal;
use sutra_contracts::campus::status::PersonKind;

/// Declares a slice status enum with `ALL`, `as_str`, `parse` and `snake_case` serde / OpenAPI names.
macro_rules! library_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

library_enum!(
    /// Borrowing policy version (draft → active → superseded).
    LibraryPolicyStatus { Draft => "draft", Active => "active", Superseded => "superseded" }
);
library_enum!(
    /// Approval state of a governed draft (`LIBRARY_POLICY_APPROVAL`).
    LibraryApprovalState { NotSubmitted => "none", Pending => "pending", Approved => "approved", Rejected => "rejected" }
);
library_enum!(
    /// Library membership (L3-G-124…126).
    LibraryMembershipStatus { Requested => "requested", Active => "active", Suspended => "suspended", Closed => "closed" }
);
library_enum!(
    /// Catalogue record (L3-G-127 / 128).
    CatalogueStatus { Draft => "draft", Published => "published", Withdrawn => "withdrawn" }
);
library_enum!(
    /// Kind of catalogued resource.
    CatalogueResourceType { Book => "book", Journal => "journal", Reference => "reference", Media => "media", Other => "other" }
);
library_enum!(
    /// Physical copy (L3-G-129 / 130).
    LibraryCopyStatus {
        Available => "available", Issued => "issued", Reserved => "reserved",
        Lost => "lost", Damaged => "damaged", Withdrawn => "withdrawn",
    }
);
library_enum!(
    /// Physical condition of a copy.
    LibraryCopyCondition { Good => "good", Fair => "fair", Poor => "poor", Damaged => "damaged" }
);
library_enum!(
    /// Loan (L3-G-131…133, 137, 139).
    LibraryLoanStatus { Active => "active", Overdue => "overdue", Returned => "returned", Lost => "lost" }
);
library_enum!(
    /// Reservation (L3-G-134…136).
    LibraryReservationStatus {
        Queued => "queued", Ready => "ready", Collected => "collected",
        Expired => "expired", Cancelled => "cancelled",
    }
);
library_enum!(
    /// Fine (L3-G-137…139).
    LibraryFineStatus {
        Assessed => "assessed", WaiverRequested => "waiver_requested", Waived => "waived",
        Billed => "billed", Paid => "paid",
    }
);
library_enum!(
    /// What a fine is for.
    LibraryFineKind { Overdue => "overdue", Replacement => "replacement" }
);
library_enum!(
    /// Library clearance (L3-G-149).
    LibraryClearanceStatus { Requested => "requested", Approved => "approved", Rejected => "rejected" }
);

impl LibraryMembershipStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use LibraryMembershipStatus::*;
        matches!(
            (self, to),
            (Requested, Active)
                | (Requested, Closed)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Active, Closed)
                | (Suspended, Closed)
        )
    }
}

impl CatalogueStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use CatalogueStatus::*;
        matches!(
            (self, to),
            (Draft, Published) | (Published, Withdrawn) | (Draft, Withdrawn)
        )
    }
}

impl LibraryCopyStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use LibraryCopyStatus::*;
        matches!(
            (self, to),
            (Available, Issued)
                | (Available, Reserved)
                | (Available, Damaged)
                | (Available, Lost)
                | (Available, Withdrawn)
                | (Issued, Available)
                | (Issued, Reserved)
                | (Issued, Lost)
                | (Reserved, Issued)
                | (Reserved, Available)
                | (Reserved, Lost)
                | (Damaged, Available)
                | (Damaged, Withdrawn)
                | (Damaged, Lost)
                | (Lost, Available)
                | (Lost, Withdrawn)
        )
    }
}

impl LibraryLoanStatus {
    /// Still on loan (counts against the borrowing limit).
    pub fn is_live(self) -> bool {
        matches!(self, Self::Active | Self::Overdue)
    }

    pub fn can_transition(self, to: Self) -> bool {
        use LibraryLoanStatus::*;
        matches!(
            (self, to),
            (Active, Overdue)
                | (Active, Returned)
                | (Overdue, Returned)
                | (Active, Lost)
                | (Overdue, Lost)
        )
    }
}

impl LibraryReservationStatus {
    pub fn is_live(self) -> bool {
        matches!(self, Self::Queued | Self::Ready)
    }

    pub fn can_transition(self, to: Self) -> bool {
        use LibraryReservationStatus::*;
        matches!(
            (self, to),
            (Queued, Ready)
                | (Queued, Cancelled)
                | (Ready, Collected)
                | (Ready, Expired)
                | (Ready, Cancelled)
        )
    }
}

impl LibraryFineStatus {
    /// Not settled: blocks borrowing and clearance (a billed fine until Finance shows it paid).
    pub fn is_unsettled(self) -> bool {
        matches!(self, Self::Assessed | Self::WaiverRequested | Self::Billed)
    }

    pub fn can_transition(self, to: Self) -> bool {
        use LibraryFineStatus::*;
        matches!(
            (self, to),
            (Assessed, WaiverRequested)
                | (WaiverRequested, Waived)
                | (WaiverRequested, Assessed)
                | (Assessed, Billed)
                | (Billed, Paid)
        )
    }
}

/// Borrowing rules of one policy version for one member kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorrowingRules {
    pub loan_days: i32,
    pub max_loans: i32,
    pub max_renewals: i32,
    pub fine_per_day: Decimal,
    pub max_fine: Option<Decimal>,
    pub hold_days: i32,
    pub lost_charge_factor: Decimal,
}

/// Rules for a member kind from the policy columns.
#[allow(clippy::too_many_arguments)]
pub fn rules_for(
    kind: PersonKind,
    loan_days_student: i32,
    loan_days_staff: i32,
    max_loans_student: i32,
    max_loans_staff: i32,
    max_renewals: i32,
    fine_per_day: Decimal,
    max_fine: Option<Decimal>,
    hold_days: i32,
    lost_charge_factor: Decimal,
) -> BorrowingRules {
    let (loan_days, max_loans) = match kind {
        PersonKind::Student => (loan_days_student, max_loans_student),
        PersonKind::Staff => (loan_days_staff, max_loans_staff),
    };
    BorrowingRules {
        loan_days,
        max_loans,
        max_renewals,
        fine_per_day,
        max_fine,
        hold_days,
        lost_charge_factor,
    }
}

/// Whole days a loan due on `due` is late on `on` (0 when not late).
pub fn days_late(due: NaiveDate, on: NaiveDate) -> i64 {
    (on - due).num_days().max(0)
}

/// Overdue fine: `days × fine_per_day`, capped by `max_fine`, rounded once (half away from zero).
pub fn overdue_fine(days: i64, fine_per_day: Decimal, max_fine: Option<Decimal>) -> Decimal {
    let raw = crate::shared::decimal::round2(Decimal::from(days.max(0)) * fine_per_day);
    match max_fine {
        Some(cap) if raw > cap => cap,
        _ => raw,
    }
}

/// Replacement obligation of a lost copy: `replacement_cost × factor`, rounded once.
pub fn replacement_charge(replacement_cost: Decimal, factor: Decimal) -> Decimal {
    crate::shared::decimal::round2(replacement_cost * factor)
}

/// `true` when `isbn` is a 10-digit (last may be `X`) or 13-digit ISBN with a valid check digit.
pub fn valid_isbn(isbn: &str) -> bool {
    let b = isbn.as_bytes();
    match b.len() {
        10 => {
            let mut sum = 0u32;
            for (i, c) in b.iter().enumerate() {
                let v = match c {
                    b'0'..=b'9' => u32::from(c - b'0'),
                    b'X' if i == 9 => 10,
                    _ => return false,
                };
                sum += v * (10 - i as u32);
            }
            sum.is_multiple_of(11)
        }
        13 => {
            if !b.iter().all(u8::is_ascii_digit) {
                return false;
            }
            let sum: u32 = b
                .iter()
                .enumerate()
                .map(|(i, c)| u32::from(c - b'0') * if i % 2 == 0 { 1 } else { 3 })
                .sum();
            sum.is_multiple_of(10)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }

    #[test]
    fn transitions() {
        use LibraryLoanStatus as L;
        assert!(L::Active.can_transition(L::Overdue));
        assert!(L::Overdue.can_transition(L::Returned));
        assert!(!L::Returned.can_transition(L::Active));
        assert!(L::Overdue.is_live() && !L::Lost.is_live());
        use LibraryFineStatus as F;
        assert!(F::Assessed.can_transition(F::WaiverRequested));
        assert!(
            !F::Billed.can_transition(F::Waived),
            "waivers happen before billing"
        );
        assert!(F::Billed.is_unsettled() && !F::Paid.is_unsettled());
        use LibraryMembershipStatus as M;
        assert!(M::Suspended.can_transition(M::Active));
        assert!(!M::Closed.can_transition(M::Active));
        use LibraryReservationStatus as R;
        assert!(R::Ready.can_transition(R::Expired));
        assert!(!R::Queued.can_transition(R::Collected));
        assert!(LibraryCopyStatus::Reserved.can_transition(LibraryCopyStatus::Issued));
        assert!(!LibraryCopyStatus::Withdrawn.can_transition(LibraryCopyStatus::Available));
        assert!(CatalogueStatus::Draft.can_transition(CatalogueStatus::Published));
        for s in LibraryFineStatus::ALL {
            assert_eq!(LibraryFineStatus::parse(s.as_str()), Some(*s));
        }
    }

    #[test]
    fn fines_are_decimal_and_capped() {
        assert_eq!(days_late(day("2026-11-10"), day("2026-11-13")), 3);
        assert_eq!(days_late(day("2026-11-10"), day("2026-11-09")), 0);
        assert_eq!(overdue_fine(3, d("2.50"), None), d("7.50"));
        assert_eq!(overdue_fine(30, d("2.50"), Some(d("50.00"))), d("50.00"));
        assert_eq!(overdue_fine(0, d("2.50"), None), d("0.00"));
        assert_eq!(replacement_charge(d("499.00"), d("1.5000")), d("748.50"));
        assert_eq!(replacement_charge(d("333.33"), d("1.0050")), d("335.00"));
    }

    #[test]
    fn rules_follow_the_member_kind() {
        let r = rules_for(
            PersonKind::Staff,
            14,
            30,
            3,
            10,
            2,
            d("1.00"),
            None,
            3,
            d("1"),
        );
        assert_eq!((r.loan_days, r.max_loans), (30, 10));
    }

    #[test]
    fn isbn_check_digits() {
        assert!(valid_isbn("9780306406157"));
        assert!(!valid_isbn("9780306406158"));
        assert!(valid_isbn("0306406152"));
        assert!(valid_isbn("080442957X"));
        assert!(!valid_isbn("12345"));
    }
}
