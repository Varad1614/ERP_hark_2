//! **8D** — suppliers, requisitions, sourcing, purchase orders, goods receipts, items / stores / stock, assets,
//! vendor invoices and payments (L2-J01…J08; L3-J-004, 007, 020, 021, 030, 035, 038…041, 043…047, 049, 051, 053, 054,
//! 057…059, 061, 064…066, 074…076, 079, 083; F-044, 045, 081, 082).
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §4.3). Payload: [`super::StatusChangedV1`] unless noted.

use super::event_type;

event_type!(
    /// Supplier approved / active (L3-J-021).
    SUPPLIER_APPROVED, SUPPLIER_APPROVED_SCHEMA, "supplier.approved");
event_type!(
    /// Supplier suspended (L3-J-030).
    SUPPLIER_SUSPENDED, SUPPLIER_SUSPENDED_SCHEMA, "supplier.suspended");
event_type!(
    /// Requisition awaiting approval (L3-J-038).
    PURCHASE_REQUISITION_SUBMITTED, PURCHASE_REQUISITION_SUBMITTED_SCHEMA, "purchase_requisition.submitted");
event_type!(
    /// Requisition approved and released (L3-J-038).
    PURCHASE_REQUISITION_APPROVED, PURCHASE_REQUISITION_APPROVED_SCHEMA, "purchase_requisition.approved");
event_type!(
    /// Requisition rejected (L3-J-039).
    PURCHASE_REQUISITION_REJECTED, PURCHASE_REQUISITION_REJECTED_SCHEMA, "purchase_requisition.rejected");
event_type!(
    /// Sourcing awarded to a supplier (L3-J-004 / 007).
    SOURCING_EVENT_AWARDED, SOURCING_EVENT_AWARDED_SCHEMA, "sourcing_event.awarded");
event_type!(
    /// Purchase order issued (L3-J-043).
    PURCHASE_ORDER_ISSUED, PURCHASE_ORDER_ISSUED_SCHEMA, "purchase_order.issued");
event_type!(
    /// Purchase order closed (L3-J-047).
    PURCHASE_ORDER_CLOSED, PURCHASE_ORDER_CLOSED_SCHEMA, "purchase_order.closed");
event_type!(
    /// Receipt inspected and accepted into stock (L3-J-049 / 054).
    GOODS_RECEIPT_ACCEPTED, GOODS_RECEIPT_ACCEPTED_SCHEMA, "goods_receipt.accepted");
event_type!(
    /// Stock issued (L3-J-064).
    STOCK_ISSUED, STOCK_ISSUED_SCHEMA, "stock_issue.issued");
event_type!(
    /// Stock at / below reorder level (L3-J-061).
    REPLENISHMENT_NEEDED, REPLENISHMENT_NEEDED_SCHEMA, "replenishment_need.raised");
event_type!(
    /// Asset registered (L3-J-074).
    ASSET_REGISTERED, ASSET_REGISTERED_SCHEMA, "asset.registered");
event_type!(
    /// Asset assigned / custody transferred (L3-J-075).
    ASSET_ASSIGNED, ASSET_ASSIGNED_SCHEMA, "asset.assigned");
event_type!(
    /// Asset retired (L3-J-083).
    ASSET_RETIRED, ASSET_RETIRED_SCHEMA, "asset.retired");
event_type!(
    /// Vendor invoice matched and posted to Finance AP (F-044 / F-081 / F-082).
    VENDOR_INVOICE_POSTED, VENDOR_INVOICE_POSTED_SCHEMA, "vendor_invoice.posted");
event_type!(
    /// Vendor payment posted to Finance (F-045).
    VENDOR_PAYMENT_POSTED, VENDOR_PAYMENT_POSTED_SCHEMA, "vendor_payment.posted");
event_type!(
    /// Part 1 (DEF-J-003): vendor payment awaiting `VENDOR_PAYMENT_APPROVAL`.
    VENDOR_PAYMENT_SUBMITTED, VENDOR_PAYMENT_SUBMITTED_SCHEMA, "vendor_payment.submitted");
event_type!(
    /// Part 1 (DEF-J-003): vendor payment approved by an independent approver (may now post).
    VENDOR_PAYMENT_APPROVED, VENDOR_PAYMENT_APPROVED_SCHEMA, "vendor_payment.approved");
event_type!(
    /// Part 1 (DEF-J-003): vendor payment rejected (never posts; its allocations are released).
    VENDOR_PAYMENT_REJECTED, VENDOR_PAYMENT_REJECTED_SCHEMA, "vendor_payment.rejected");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    SUPPLIER_APPROVED,
    SUPPLIER_SUSPENDED,
    PURCHASE_REQUISITION_SUBMITTED,
    PURCHASE_REQUISITION_APPROVED,
    PURCHASE_REQUISITION_REJECTED,
    SOURCING_EVENT_AWARDED,
    PURCHASE_ORDER_ISSUED,
    PURCHASE_ORDER_CLOSED,
    GOODS_RECEIPT_ACCEPTED,
    STOCK_ISSUED,
    REPLENISHMENT_NEEDED,
    ASSET_REGISTERED,
    ASSET_ASSIGNED,
    ASSET_RETIRED,
    VENDOR_INVOICE_POSTED,
    VENDOR_PAYMENT_POSTED,
    VENDOR_PAYMENT_SUBMITTED,
    VENDOR_PAYMENT_APPROVED,
    VENDOR_PAYMENT_REJECTED,
];
