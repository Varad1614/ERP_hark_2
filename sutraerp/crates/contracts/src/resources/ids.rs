//! Typed UUIDv7 ids of the Batch 8 Domain J (Enterprise resources) aggregates.
//!
//! Foundation-owned and **append-only** (`docs/batch8-contracts.md` §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared Resources aggregates ----
typed_id!(
    /// Item master record (L3-J-051).
    ItemId
);
typed_id!(
    /// Store / warehouse or a location inside one (L3-J-053).
    StoreLocationId
);
typed_id!(
    /// One append-only stock ledger movement (L3-J-058).
    StockLedgerEntryId
);

// ---- 8D: procurement, inventory, assets, payables ----
typed_id!(
    /// Supplier / vendor (L2-J02).
    SupplierId
);
typed_id!(
    /// Purchase requisition (L2-J03).
    PurchaseRequisitionId
);
typed_id!(
    /// RFQ / direct sourcing event (L3-J-004 / 007).
    SourcingEventId
);
typed_id!(
    /// Supplier quotation of a sourcing event (L3-J-004).
    QuotationId
);
typed_id!(
    /// Purchase order (L2-J04).
    PurchaseOrderId
);
typed_id!(
    /// Goods receipt + inspection (L2-J05).
    GoodsReceiptId
);
typed_id!(
    /// Stock issue to a requester / department (L3-J-064).
    StockIssueId
);
typed_id!(
    /// Stock reservation (L3-J-065).
    StockReservationId
);
typed_id!(
    /// Return of issued stock to the store (L3-J-066).
    StockReturnId
);
typed_id!(
    /// Approved stock correction (L3-J-059).
    StockAdjustmentId
);
typed_id!(
    /// Replenishment need from a reorder level (L3-J-061).
    ReplenishmentNeedId
);
typed_id!(
    /// Operational asset (L2-J08).
    AssetId
);
typed_id!(
    /// Assignment / custody of an asset (L3-J-075 / 076).
    AssetAssignmentId
);
typed_id!(
    /// Vendor invoice matched against PO + receipt (F-044 / F-081).
    VendorInvoiceId
);
typed_id!(
    /// Vendor payment settling invoices (F-045).
    VendorPaymentId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_uuid_v7_and_transparent() {
        let id = ItemId::new();
        assert_eq!(id.as_uuid().get_version_num(), 7);
        let json = serde_json::to_string(&id).unwrap_or_default();
        assert_eq!(json, format!("\"{id}\""));
    }
}
