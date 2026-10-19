-- Batch 8 foundation — Domain J (Enterprise resources) shared tables (docs/batch8-contracts.md §3).
--
-- Slice 8D adds its tables in 20261115000070-089 (resources.<what>) and never edits this file.
-- Quantities are NUMERIC(14,3), money NUMERIC(14,2) (rust_decimal — never floats).
--
-- Write rules (§3.3):
--   item, store_location  8D only (own repo code; tests seed via support::b8).
--   stock_ledger_entry    8D ONLY via `shared::stock::post_movement` (append-only; idempotent on
--                         (source_type, source_id, posting_key)); on-hand = projection `shared::stock::on_hand`.

-- Append-only guard reused by every resources history / ledger table (slices reuse it too).
CREATE FUNCTION resources.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of resources.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

-- ------------------------------------------------------------------ item master (L3-J-051)
CREATE TABLE resources.item (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_.-]{0,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    kind                 text         NOT NULL CHECK (kind IN ('consumable', 'asset', 'service')),
    category             text         CHECK (category IS NULL OR length(btrim(category)) BETWEEN 1 AND 100),
    unit                 text         NOT NULL DEFAULT 'each' CHECK (length(btrim(unit)) BETWEEN 1 AND 16),
    reorder_level        numeric(14,3) CHECK (reorder_level IS NULL OR reorder_level >= 0),
    reorder_quantity     numeric(14,3) CHECK (reorder_quantity IS NULL OR reorder_quantity > 0),
    standard_cost        numeric(14,2) CHECK (standard_cost IS NULL OR standard_cost >= 0),
    status               text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'inactive', 'archived')),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX item_code ON resources.item (tenant_id, institution_id, code);

-- ------------------------------------------------------------------ stores and locations (L3-J-053)
CREATE TABLE resources.store_location (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    -- NULL = a store (warehouse); else a bin / rack / room inside that store.
    parent_id            uuid         REFERENCES resources.store_location (id),
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_.-]{0,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    -- Domain G infrastructure node the store sits in (plain uuid, optional).
    campus_node_id       uuid,
    status               text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE UNIQUE INDEX store_location_code ON resources.store_location (tenant_id, institution_id, code);

-- ------------------------------------------------------------------ stock ledger (L3-J-058, append-only)
CREATE TABLE resources.stock_ledger_entry (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    item_id              uuid         NOT NULL REFERENCES resources.item (id),
    location_id          uuid         NOT NULL REFERENCES resources.store_location (id),
    kind                 text         NOT NULL CHECK (kind IN ('receipt', 'issue', 'return_to_store', 'transfer_out',
                                          'transfer_in', 'adjustment', 'reversal')),
    -- Signed: + into the location, − out of it (sign fixed by the kind, see StockMovementKind::sign).
    quantity             numeric(14,3) NOT NULL CHECK (quantity <> 0),
    unit_cost            numeric(14,2) CHECK (unit_cost IS NULL OR unit_cost >= 0),
    effective_on         date         NOT NULL,
    -- Idempotency key: (source_type, source_id, posting_key) unique per tenant.
    source_type          text         NOT NULL CHECK (source_type ~ '^[a-z][a-z0-9_]{1,47}$'),
    source_id            uuid         NOT NULL,
    posting_key          text         NOT NULL CHECK (length(btrim(posting_key)) BETWEEN 1 AND 120),
    reverses_entry_id    uuid         REFERENCES resources.stock_ledger_entry (id),
    reason               text,
    actor                text,
    correlation_id       text,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (CASE kind
              WHEN 'receipt' THEN quantity > 0
              WHEN 'return_to_store' THEN quantity > 0
              WHEN 'transfer_in' THEN quantity > 0
              WHEN 'issue' THEN quantity < 0
              WHEN 'transfer_out' THEN quantity < 0
              ELSE true END)
);
CREATE UNIQUE INDEX stock_ledger_entry_idempotency
    ON resources.stock_ledger_entry (tenant_id, source_type, source_id, posting_key);
CREATE INDEX stock_ledger_entry_item ON resources.stock_ledger_entry (tenant_id, item_id, location_id);
CREATE TRIGGER stock_ledger_entry_append_only BEFORE UPDATE OR DELETE ON resources.stock_ledger_entry
    FOR EACH ROW EXECUTE FUNCTION resources.forbid_history_change();

-- ------------------------------------------------------------------ numbering + history
CREATE TABLE resources.number_sequence (
    tenant_id        uuid    NOT NULL,
    institution_id   uuid    NOT NULL,
    kind             text    NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{1,47}$'),
    next_value       bigint  NOT NULL CHECK (next_value >= 1),
    PRIMARY KEY (tenant_id, institution_id, kind)
);

CREATE TABLE resources.status_history (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    aggregate_type       text         NOT NULL,
    aggregate_id         uuid         NOT NULL,
    from_status          text,
    to_status            text         NOT NULL,
    reason               text,
    cause_event_type     text,
    process_instance_id  text,
    actor                text,
    correlation_id       text,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX resources_status_history_aggregate_idx
    ON resources.status_history (tenant_id, aggregate_type, aggregate_id, created_at);
CREATE TRIGGER resources_status_history_append_only BEFORE UPDATE OR DELETE ON resources.status_history
    FOR EACH ROW EXECUTE FUNCTION resources.forbid_history_change();

GRANT USAGE ON SCHEMA resources TO sutra_resources;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA resources TO sutra_resources;
REVOKE UPDATE, DELETE, TRUNCATE ON resources.stock_ledger_entry, resources.status_history FROM sutra_resources;
GRANT INSERT ON platform.outbox, platform.audit_log TO sutra_resources;
