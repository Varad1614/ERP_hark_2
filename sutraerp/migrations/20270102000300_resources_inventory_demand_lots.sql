-- ERP_EXTENSIONS 2C — inventory demand and dated lots. Logic ported from HeyPuter/everything
-- inventory src/lib/demand.js, reorder.js, lots.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- Items gain planning fields (supplier lead time, pack size, safety days, expiry tracking).
-- Dated lots are a projection kept by the single ledger writer (shared::stock::post_movement):
-- receipts add a lot (dated when the goods receipt line says), issues consume first-expired-
-- first-out, and every ledger entry records the lots it moved so transfers and reversals carry
-- dates across. Demand settings are per institution.
ALTER TABLE resources.item
    ADD COLUMN lead_time_days   integer       CHECK (lead_time_days IS NULL OR lead_time_days BETWEEN 0 AND 365),
    ADD COLUMN pack_size        numeric(14,3) CHECK (pack_size IS NULL OR pack_size > 0),
    ADD COLUMN safety_days      integer       CHECK (safety_days IS NULL OR safety_days BETWEEN 0 AND 365),
    ADD COLUMN track_expiry     boolean       NOT NULL DEFAULT false;

ALTER TABLE resources.goods_receipt_line
    ADD COLUMN expiry_on        date;

CREATE TABLE resources.stock_lot (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    item_id              uuid          NOT NULL REFERENCES resources.item (id),
    location_id          uuid          NOT NULL REFERENCES resources.store_location (id),
    -- NULL = date unknown (stock from before tracking, or a receipt that did not say).
    expiry_on            date,
    quantity             numeric(14,3) NOT NULL CHECK (quantity > 0),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX stock_lot_key ON resources.stock_lot
    (tenant_id, item_id, location_id, coalesce(expiry_on, 'infinity'::date));
CREATE INDEX stock_lot_expiry_idx ON resources.stock_lot (tenant_id, institution_id, expiry_on);

-- The dated quantities each ledger entry moved (signed like the entry). Append-only.
CREATE TABLE resources.stock_lot_movement (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    ledger_entry_id      uuid          NOT NULL REFERENCES resources.stock_ledger_entry (id),
    expiry_on            date,
    quantity             numeric(14,3) NOT NULL CHECK (quantity <> 0),
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX stock_lot_movement_entry_idx ON resources.stock_lot_movement (tenant_id, ledger_entry_id);
REVOKE UPDATE, DELETE, TRUNCATE ON resources.stock_lot_movement FROM sutra_resources;

CREATE TABLE resources.inventory_demand_settings (
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    demand_days          integer       NOT NULL DEFAULT 30 CHECK (demand_days BETWEEN 7 AND 365),
    safety_days          integer       NOT NULL DEFAULT 3 CHECK (safety_days BETWEEN 0 AND 365),
    coverage_days        integer       NOT NULL DEFAULT 0 CHECK (coverage_days BETWEEN 0 AND 365),
    expiry_days          integer       NOT NULL DEFAULT 30 CHECK (expiry_days BETWEEN 1 AND 365),
    version              bigint        NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON resources.stock_lot, resources.inventory_demand_settings
    TO sutra_resources;
GRANT SELECT, INSERT ON resources.stock_lot_movement TO sutra_resources;
SELECT platform.apply_tenant_rls();
