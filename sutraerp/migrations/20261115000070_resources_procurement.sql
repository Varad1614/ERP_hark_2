-- Batch 8 slice 8D — procurement: suppliers, purchase requisitions (+ lines), sourcing events
-- (RFQ / direct) with quotations (+ lines), purchase orders (+ lines)
-- (docs/batch8-contracts.md §3; L2-J02 … J04; L3-J-004, 007, 020, 021, 030, 035, 038 … 041, 043 … 047).
--
-- Owned by 8D. Money NUMERIC(14,2), quantities NUMERIC(14,3), rates NUMERIC(7,4) (rust_decimal —
-- never floats). Other domains' ids (org units, staff, journal entries) are plain uuids validated
-- through the ports; FKs only to resources tables.

-- ------------------------------------------------------------------ suppliers (J-020 / 021 / 030 / 035)
CREATE TABLE resources.supplier (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    legal_name            text         NOT NULL CHECK (length(btrim(legal_name)) BETWEEN 1 AND 200),
    trade_name            text,
    category              text,
    gstin                 text         CHECK (gstin IS NULL OR gstin ~ '^[0-9]{2}[A-Z]{5}[0-9]{4}[A-Z][1-9A-Z]Z[0-9A-Z]$'),
    pan                   text         CHECK (pan IS NULL OR pan ~ '^[A-Z]{5}[0-9]{4}[A-Z]$'),
    email                 text,
    phone                 text,
    address               text,
    bank_account_name     text,
    bank_account_number   text,
    bank_ifsc             text         CHECK (bank_ifsc IS NULL OR bank_ifsc ~ '^[A-Z]{4}0[A-Z0-9]{6}$'),
    status                text         NOT NULL DEFAULT 'prospective' CHECK (status IN ('prospective', 'registered',
                                           'approved', 'active', 'suspended', 'merged', 'inactive')),
    status_reason         text,
    approval_pending      boolean      NOT NULL DEFAULT false,
    canonical_supplier_id uuid         REFERENCES resources.supplier (id),
    process_instance_id   text,
    registered_at         timestamptz,
    approved_at           timestamptz,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now(),
    CHECK (canonical_supplier_id IS NULL OR canonical_supplier_id <> id),
    CHECK ((status = 'merged') = (canonical_supplier_id IS NOT NULL))
);
CREATE UNIQUE INDEX supplier_number ON resources.supplier (tenant_id, institution_id, number);
CREATE INDEX supplier_gstin ON resources.supplier (tenant_id, institution_id, gstin) WHERE gstin IS NOT NULL;

-- ------------------------------------------------------------------ purchase requisitions (J-038 … 041)
CREATE TABLE resources.purchase_requisition (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    title                 text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    justification         text,
    -- Domain B org unit / Domain E staff member (plain uuids, validated through the ports).
    org_unit_id           uuid,
    requester_staff_id    uuid,
    requester_subject     text,
    needed_by             date,
    revision              integer      NOT NULL DEFAULT 1 CHECK (revision >= 1),
    estimated_total       numeric(14,2) NOT NULL DEFAULT 0 CHECK (estimated_total >= 0),
    status                text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'approved',
                                           'released', 'rejected', 'amended', 'withdrawn', 'cancelled', 'ordered', 'closed')),
    status_reason         text,
    -- Replenishment run that drafted it (L3-J-061), when any.
    source                text         NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'replenishment')),
    process_instance_id   text,
    submitted_at          timestamptz,
    decided_at            timestamptz,
    decided_by            text,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX purchase_requisition_number ON resources.purchase_requisition (tenant_id, institution_id, number);

CREATE TABLE resources.purchase_requisition_line (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    requisition_id        uuid         NOT NULL REFERENCES resources.purchase_requisition (id) ON DELETE CASCADE,
    line_no               integer      NOT NULL CHECK (line_no >= 1),
    item_id               uuid         REFERENCES resources.item (id),
    description           text         NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 300),
    quantity              numeric(14,3) NOT NULL CHECK (quantity > 0),
    unit                  text         NOT NULL DEFAULT 'each',
    estimated_unit_cost   numeric(14,2) NOT NULL CHECK (estimated_unit_cost >= 0),
    estimated_amount      numeric(14,2) NOT NULL CHECK (estimated_amount >= 0),
    UNIQUE (requisition_id, line_no)
);

-- ------------------------------------------------------------------ sourcing (J-004 / 007)
CREATE TABLE resources.sourcing_event (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    requisition_id        uuid         NOT NULL REFERENCES resources.purchase_requisition (id),
    method                text         NOT NULL CHECK (method IN ('rfq', 'direct')),
    title                 text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    closing_date          date,
    estimated_value       numeric(14,2) NOT NULL DEFAULT 0,
    status                text         NOT NULL DEFAULT 'open' CHECK (status IN ('draft', 'open', 'evaluating',
                                           'awarded', 'cancelled')),
    status_reason         text,
    -- Award awaiting AWARD_APPROVAL (AwaitDecision) / the awarded quotation.
    pending_quotation_id  uuid,
    awarded_quotation_id  uuid,
    awarded_supplier_id   uuid         REFERENCES resources.supplier (id),
    award_reason          text,
    process_instance_id   text,
    awarded_at            timestamptz,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX sourcing_event_number ON resources.sourcing_event (tenant_id, institution_id, number);

CREATE TABLE resources.quotation (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    sourcing_event_id     uuid         NOT NULL REFERENCES resources.sourcing_event (id),
    supplier_id           uuid         NOT NULL REFERENCES resources.supplier (id),
    reference             text,
    total_amount          numeric(14,2) NOT NULL CHECK (total_amount >= 0),
    delivery_days         integer      CHECK (delivery_days IS NULL OR delivery_days >= 0),
    valid_until           date,
    rank                  integer,
    status                text         NOT NULL DEFAULT 'received' CHECK (status IN ('received', 'shortlisted',
                                           'accepted', 'rejected')),
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (sourcing_event_id, supplier_id)
);

CREATE TABLE resources.quotation_line (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    quotation_id          uuid         NOT NULL REFERENCES resources.quotation (id) ON DELETE CASCADE,
    line_no               integer      NOT NULL CHECK (line_no >= 1),
    unit_price            numeric(14,2) NOT NULL CHECK (unit_price >= 0),
    amount                numeric(14,2) NOT NULL CHECK (amount >= 0),
    UNIQUE (quotation_id, line_no)
);

-- ------------------------------------------------------------------ purchase orders (J-043 … 047)
CREATE TABLE resources.purchase_order (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    number                text         NOT NULL,
    supplier_id           uuid         NOT NULL REFERENCES resources.supplier (id),
    requisition_id        uuid         REFERENCES resources.purchase_requisition (id),
    sourcing_event_id     uuid         REFERENCES resources.sourcing_event (id),
    quotation_id          uuid         REFERENCES resources.quotation (id),
    delivery_location_id  uuid         REFERENCES resources.store_location (id),
    order_date            date         NOT NULL,
    expected_delivery     date,
    revision              integer      NOT NULL DEFAULT 1 CHECK (revision >= 1),
    total_amount          numeric(14,2) NOT NULL DEFAULT 0 CHECK (total_amount >= 0),
    notes                 text,
    status                text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'issued', 'acknowledged',
                                           'partially_received', 'received', 'closed', 'cancelled')),
    status_reason         text,
    approval_pending      boolean      NOT NULL DEFAULT false,
    process_instance_id   text,
    issued_at             timestamptz,
    acknowledged_at       timestamptz,
    acknowledgement_ref   text,
    closed_at             timestamptz,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX purchase_order_number ON resources.purchase_order (tenant_id, institution_id, number);

CREATE TABLE resources.purchase_order_line (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    purchase_order_id     uuid         NOT NULL REFERENCES resources.purchase_order (id) ON DELETE CASCADE,
    line_no               integer      NOT NULL CHECK (line_no >= 1),
    item_id               uuid         REFERENCES resources.item (id),
    -- stock: consumable items (receipt movements, 1410); asset: asset items (asset candidates, 1410);
    -- service: free text / service items (5510).
    line_kind             text         NOT NULL CHECK (line_kind IN ('stock', 'asset', 'service')),
    description           text         NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 300),
    quantity              numeric(14,3) NOT NULL CHECK (quantity > 0),
    unit                  text         NOT NULL DEFAULT 'each',
    unit_price            numeric(14,2) NOT NULL CHECK (unit_price >= 0),
    amount                numeric(14,2) NOT NULL CHECK (amount >= 0),
    received_quantity     numeric(14,3) NOT NULL DEFAULT 0 CHECK (received_quantity >= 0),
    invoiced_quantity     numeric(14,3) NOT NULL DEFAULT 0,
    UNIQUE (purchase_order_id, line_no),
    CHECK (received_quantity <= quantity)
);
