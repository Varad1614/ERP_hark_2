//! Lifecycle state and classification enums of the Batch 8 Domain J (Enterprise resources)
//! aggregates.
//!
//! Foundation-owned (`docs/batch8-contracts.md` §4.2). The shared append-only stock ledger
//! carries its sign rule here; every other machine's transitions are slice-owned (8D `domain.rs`).

/// Declares a status enum with `ALL`, `as_str`, `parse` and serde/OpenAPI `snake_case` names.
macro_rules! status_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

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

// ================================================================ foundation

status_enum!(
    /// Item master lifecycle (L3-J-051 / 052).
    ItemStatus {
        Draft => "draft",
        Active => "active",
        Inactive => "inactive",
        Archived => "archived",
    }
);

status_enum!(
    /// What an item is (stocked consumable, capital asset candidate, service).
    ItemKind {
        /// Stocked consumable (stationery, lab chemicals, spares).
        Consumable => "consumable",
        /// Capital item: accepted units become assets (J08).
        Asset => "asset",
        /// Service line on a PO (no stock).
        Service => "service",
    }
);

status_enum!(
    /// Store / location status (L3-J-053).
    StoreLocationStatus {
        Active => "active",
        Inactive => "inactive",
    }
);

status_enum!(
    /// Kind of an append-only stock ledger movement (L3-J-058). The sign of `quantity` is fixed
    /// by the kind ([`StockMovementKind::sign`]).
    StockMovementKind {
        /// Accepted goods receipt (J05 → J06).
        Receipt => "receipt",
        /// Issue to a requester / department (J07).
        Issue => "issue",
        /// Return of issued stock to the store.
        ReturnToStore => "return_to_store",
        TransferOut => "transfer_out",
        TransferIn => "transfer_in",
        /// Approved correction, either sign.
        Adjustment => "adjustment",
        /// Reversal of an earlier movement, either sign.
        Reversal => "reversal",
    }
);

impl StockMovementKind {
    /// `1` positive only, `-1` negative only, `0` either sign.
    pub fn sign(self) -> i8 {
        match self {
            Self::Receipt | Self::ReturnToStore | Self::TransferIn => 1,
            Self::Issue | Self::TransferOut => -1,
            Self::Adjustment | Self::Reversal => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_signs_and_names() {
        assert_eq!(StockMovementKind::Receipt.sign(), 1);
        assert_eq!(StockMovementKind::Issue.sign(), -1);
        assert_eq!(StockMovementKind::Adjustment.sign(), 0);
        for k in StockMovementKind::ALL {
            assert_eq!(StockMovementKind::parse(k.as_str()), Some(*k));
        }
        assert_eq!(ItemKind::parse("asset"), Some(ItemKind::Asset));
        assert_eq!(ItemStatus::parse("active"), Some(ItemStatus::Active));
        assert_eq!(StoreLocationStatus::ALL.len(), 2);
    }
}
