-- Batch 8 slice 8D — goods receipts (+ lines), stock issues / reservations / returns /
-- adjustments, replenishment needs, assets (+ assignments), vendor invoices (+ lines) and vendor
-- payments (+ allocations) (docs/batch8-contracts.md §3; L2-J05 … J08; L3-J-049, 051, 053, 054,
-- 057 … 059, 061, 064 … 066, 074 … 076, 079, 083; F-044, 045, 081, 082).
--
-- Owned by 8D. Every stock movement goes through the foundation ledger
-- (`shared::stock::post_movement`); these tables only keep the business documents and the ledger
-- entry ids. Finance postings go through `ProcurementPostingPort` (journal ids stored plain).

-- ------------------------------------------------------------------ goods receipts (J-049 / 054 / 057)
CREATE TABLE resources.goods_receipt (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    purchase_order_id     uuid         NOT NULL REFERENCES resources.purchase_order (id),
    supplier_id           uuid         NOT NULL REFERENCES resources.supplier (id),
    location_id           uuid         NOT NULL REFERENCES resources.store_location (id),
    received_on           date         NOT NULL,
    delivery_note         text,
    status                text         NOT NULL DEFAULT 'received' CHECK (status IN ('received', 'inspecting',
                                           'accepted', 'partially_accepted', 'rejected')),
    status_reason         text,
    inspected_at          timestamptz,
    decided_at            timestamptz,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX goods_receipt_number ON resources.goods_receipt (tenant_id, institution_id, number);

CREATE TABLE resources.goods_receipt_line (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    goods_receipt_id      uuid         NOT NULL REFERENCES resources.goods_receipt (id),
    line_no               integer      NOT NULL CHECK (line_no >= 1),
    purchase_order_line_id uuid        NOT NULL REFERENCES resources.purchase_order_line (id),
    item_id               uuid         REFERENCES resources.item (id),
    line_kind             text         NOT NULL CHECK (line_kind IN ('stock', 'asset', 'service')),
    received_quantity     numeric(14,3) NOT NULL CHECK (received_quantity > 0),
    accepted_quantity     numeric(14,3) NOT NULL DEFAULT 0 CHECK (accepted_quantity >= 0),
    rejected_quantity     numeric(14,3) NOT NULL DEFAULT 0 CHECK (rejected_quantity >= 0),
    rejection_reason      text,
    unit_cost             numeric(14,2) NOT NULL DEFAULT 0,
    ledger_entry_id       uuid         REFERENCES resources.stock_ledger_entry (id),
    UNIQUE (goods_receipt_id, line_no),
    CHECK (accepted_quantity + rejected_quantity <= received_quantity)
);

-- ------------------------------------------------------------------ stock issues + returns (J-064 / 066)
CREATE TABLE resources.stock_issue (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    item_id               uuid         NOT NULL REFERENCES resources.item (id),
    location_id           uuid         NOT NULL REFERENCES resources.store_location (id),
    quantity              numeric(14,3) NOT NULL CHECK (quantity > 0),
    returned_quantity     numeric(14,3) NOT NULL DEFAULT 0 CHECK (returned_quantity >= 0),
    -- Who / what it goes to: a Domain E staff member and / or a Domain B org unit (plain uuids).
    issued_to_staff_id    uuid,
    org_unit_id           uuid,
    purpose               text         NOT NULL CHECK (length(btrim(purpose)) BETWEEN 1 AND 300),
    reservation_id        uuid,
    status                text         NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'approved',
                                           'issued', 'returned', 'cancelled')),
    status_reason         text,
    ledger_entry_id       uuid         REFERENCES resources.stock_ledger_entry (id),
    issued_on             date,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now(),
    CHECK (returned_quantity <= quantity)
);
CREATE UNIQUE INDEX stock_issue_number ON resources.stock_issue (tenant_id, institution_id, number);

CREATE TABLE resources.stock_return (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    stock_issue_id        uuid         NOT NULL REFERENCES resources.stock_issue (id),
    quantity              numeric(14,3) NOT NULL CHECK (quantity > 0),
    reason                text,
    returned_on           date         NOT NULL,
    ledger_entry_id       uuid         NOT NULL REFERENCES resources.stock_ledger_entry (id),
    actor                 text,
    created_at            timestamptz  NOT NULL DEFAULT now()
);

-- ------------------------------------------------------------------ reservations (J-065)
CREATE TABLE resources.stock_reservation (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    item_id               uuid         NOT NULL REFERENCES resources.item (id),
    location_id           uuid         NOT NULL REFERENCES resources.store_location (id),
    quantity              numeric(14,3) NOT NULL CHECK (quantity > 0),
    purpose               text         NOT NULL CHECK (length(btrim(purpose)) BETWEEN 1 AND 300),
    needed_on             date,
    status                text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'released', 'fulfilled')),
    status_reason         text,
    stock_issue_id        uuid         REFERENCES resources.stock_issue (id),
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX stock_reservation_active ON resources.stock_reservation (tenant_id, item_id, location_id)
    WHERE status = 'active';

-- ------------------------------------------------------------------ adjustments (J-059)
CREATE TABLE resources.stock_adjustment (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    item_id               uuid         NOT NULL REFERENCES resources.item (id),
    location_id           uuid         NOT NULL REFERENCES resources.store_location (id),
    -- Signed correction (+ found / − lost or damaged).
    quantity              numeric(14,3) NOT NULL CHECK (quantity <> 0),
    reason                text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    status                text         NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'approved',
                                           'applied', 'rejected')),
    status_reason         text,
    process_instance_id   text,
    ledger_entry_id       uuid         REFERENCES resources.stock_ledger_entry (id),
    decided_by            text,
    applied_on            date,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);

-- ------------------------------------------------------------------ replenishment (J-061)
CREATE TABLE resources.replenishment_need (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    item_id               uuid         NOT NULL REFERENCES resources.item (id),
    on_hand               numeric(14,3) NOT NULL,
    reserved              numeric(14,3) NOT NULL DEFAULT 0,
    reorder_level         numeric(14,3) NOT NULL,
    suggested_quantity    numeric(14,3) NOT NULL CHECK (suggested_quantity > 0),
    status                text         NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'requisitioned', 'closed')),
    requisition_id        uuid         REFERENCES resources.purchase_requisition (id),
    run_key               text         NOT NULL,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
-- One open need per item.
CREATE UNIQUE INDEX replenishment_need_open ON resources.replenishment_need (tenant_id, item_id)
    WHERE status = 'open';

-- ------------------------------------------------------------------ assets (J-074 … 076, 079, 083)
CREATE TABLE resources.asset (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    name                  text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    item_id               uuid         REFERENCES resources.item (id),
    goods_receipt_line_id uuid         REFERENCES resources.goods_receipt_line (id),
    serial_number         text,
    category              text,
    acquisition_cost      numeric(14,2) CHECK (acquisition_cost IS NULL OR acquisition_cost >= 0),
    acquired_on           date,
    condition             text         NOT NULL DEFAULT 'good' CHECK (condition IN ('new', 'good', 'fair', 'poor', 'damaged')),
    status                text         NOT NULL DEFAULT 'registered' CHECK (status IN ('registered', 'available',
                                           'assigned', 'under_repair', 'retired')),
    status_reason         text,
    -- Current custody: a Domain E staff member or a Domain G space (plain uuids).
    custodian_kind        text         CHECK (custodian_kind IS NULL OR custodian_kind IN ('staff', 'space')),
    custodian_id          uuid,
    custodian_subject     text,
    retirement_pending    boolean      NOT NULL DEFAULT false,
    process_instance_id   text,
    retired_on            date,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now(),
    CHECK (status <> 'assigned' OR custodian_id IS NOT NULL)
);
CREATE UNIQUE INDEX asset_number ON resources.asset (tenant_id, institution_id, number);
CREATE UNIQUE INDEX asset_serial ON resources.asset (tenant_id, institution_id, serial_number)
    WHERE serial_number IS NOT NULL;

CREATE TABLE resources.asset_assignment (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    asset_id              uuid         NOT NULL REFERENCES resources.asset (id),
    assignee_kind         text         NOT NULL CHECK (assignee_kind IN ('staff', 'space')),
    assignee_id           uuid         NOT NULL,
    assigned_on           date         NOT NULL,
    returned_on           date,
    condition_out         text         NOT NULL,
    condition_in          text,
    note                  text,
    actor                 text,
    created_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX asset_assignment_open ON resources.asset_assignment (asset_id) WHERE returned_on IS NULL;

-- ------------------------------------------------------------------ vendor invoices (F-044 / 081 / 082)
CREATE TABLE resources.vendor_invoice (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    supplier_id           uuid         NOT NULL REFERENCES resources.supplier (id),
    purchase_order_id     uuid         NOT NULL REFERENCES resources.purchase_order (id),
    supplier_invoice_number text       NOT NULL CHECK (length(btrim(supplier_invoice_number)) BETWEEN 1 AND 60),
    is_credit_note        boolean      NOT NULL DEFAULT false,
    invoice_date          date         NOT NULL,
    due_date              date,
    -- Signed (credit notes negative): stock + asset lines → 1410, service lines → 5510.
    inventory_amount      numeric(14,2) NOT NULL DEFAULT 0,
    expense_amount        numeric(14,2) NOT NULL DEFAULT 0,
    total_amount          numeric(14,2) NOT NULL DEFAULT 0,
    paid_amount           numeric(14,2) NOT NULL DEFAULT 0,
    status                text         NOT NULL DEFAULT 'received' CHECK (status IN ('received', 'matched', 'mismatch',
                                           'approved', 'posted', 'paid', 'cancelled')),
    status_reason         text,
    -- Three-way match findings (F-081): [{lineNo, code, message}].
    match_findings        jsonb        NOT NULL DEFAULT '[]'::jsonb,
    matched_at            timestamptz,
    approval_pending      boolean      NOT NULL DEFAULT false,
    process_instance_id   text,
    journal_entry_id      uuid,
    entry_number          text,
    posted_at             timestamptz,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX vendor_invoice_number ON resources.vendor_invoice (tenant_id, institution_id, number);
CREATE UNIQUE INDEX vendor_invoice_supplier_ref ON resources.vendor_invoice (tenant_id, supplier_id, supplier_invoice_number)
    WHERE status <> 'cancelled';

CREATE TABLE resources.vendor_invoice_line (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    vendor_invoice_id     uuid         NOT NULL REFERENCES resources.vendor_invoice (id),
    line_no               integer      NOT NULL CHECK (line_no >= 1),
    purchase_order_line_id uuid        NOT NULL REFERENCES resources.purchase_order_line (id),
    line_kind             text         NOT NULL CHECK (line_kind IN ('stock', 'asset', 'service')),
    quantity              numeric(14,3) NOT NULL CHECK (quantity > 0),
    unit_price            numeric(14,2) NOT NULL CHECK (unit_price >= 0),
    amount                numeric(14,2) NOT NULL CHECK (amount >= 0),
    UNIQUE (vendor_invoice_id, line_no)
);

-- ------------------------------------------------------------------ vendor payments (F-045)
CREATE TABLE resources.vendor_payment (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    supplier_id           uuid         NOT NULL REFERENCES resources.supplier (id),
    payment_date          date         NOT NULL,
    amount                numeric(14,2) NOT NULL CHECK (amount > 0),
    reference             text,
    status                text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'posted')),
    journal_entry_id      uuid,
    entry_number          text,
    posted_at             timestamptz,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX vendor_payment_number ON resources.vendor_payment (tenant_id, institution_id, number);

CREATE TABLE resources.vendor_payment_allocation (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    vendor_payment_id     uuid         NOT NULL REFERENCES resources.vendor_payment (id),
    vendor_invoice_id     uuid         NOT NULL REFERENCES resources.vendor_invoice (id),
    amount                numeric(14,2) NOT NULL CHECK (amount > 0),
    UNIQUE (vendor_payment_id, vendor_invoice_id)
);

-- Returns to store are evidence: append-only.
REVOKE UPDATE, DELETE, TRUNCATE ON resources.stock_return FROM sutra_resources;
CREATE TRIGGER stock_return_append_only BEFORE UPDATE OR DELETE ON resources.stock_return
    FOR EACH ROW EXECUTE FUNCTION resources.forbid_history_change();
