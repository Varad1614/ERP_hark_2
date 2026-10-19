-- Batch 9 foundation — tamper-evident audit chaining (ADR-L5-0012, docs/batch9-contracts.md §4).
--
-- `platform.audit_log` stays exactly as it is (every module keeps inserting through
-- `sutra_persistence::audit::insert` inside its own transaction). A single sealer (9A,
-- advisory-locked, `sutra_persistence::audit_chain`) appends every not-yet-sealed audit row, per
-- tenant and in seal order, to `audit_chain_entry` with
--     entry_hash = sha256('sutra.audit.v1' || E'\n' || hex(prev_hash) || E'\n' || canonical_json(row))
-- (`sutra_kernel::audit_chain::entry_hash`), moves the tenant's head, and periodically signs the
-- head into `audit_anchor` (HMAC-SHA-256, key from OpenBao). Entries and anchors are append-only
-- by trigger and by privilege; only the head row moves.

CREATE TABLE platform.audit_chain_entry (
    tenant_id   uuid        NOT NULL,
    seq         bigint      NOT NULL CHECK (seq >= 1),
    audit_id    uuid        NOT NULL UNIQUE REFERENCES platform.audit_log (id),
    prev_hash   bytea       NOT NULL CHECK (octet_length(prev_hash) = 32),
    entry_hash  bytea       NOT NULL CHECK (octet_length(entry_hash) = 32),
    sealed_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, seq)
);

CREATE TABLE platform.audit_chain_head (
    tenant_id   uuid        PRIMARY KEY,
    seq         bigint      NOT NULL CHECK (seq >= 1),
    head_hash   bytea       NOT NULL CHECK (octet_length(head_hash) = 32),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE platform.audit_anchor (
    id          uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id   uuid        NOT NULL,
    seq         bigint      NOT NULL CHECK (seq >= 1),
    head_hash   bytea       NOT NULL CHECK (octet_length(head_hash) = 32),
    key_id      text        NOT NULL,
    mac         bytea       NOT NULL CHECK (octet_length(mac) = 32),
    anchored_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, seq, key_id)
);

CREATE INDEX audit_anchor_tenant_time_idx ON platform.audit_anchor (tenant_id, anchored_at DESC);

-- The sealer finds unsealed rows with an anti-join on audit_id; keep it cheap.
CREATE INDEX audit_log_tenant_id_idx ON platform.audit_log (tenant_id, id);

CREATE FUNCTION platform.audit_chain_immutable() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME;
END;
$$;

CREATE TRIGGER audit_chain_entry_no_update_delete
    BEFORE UPDATE OR DELETE ON platform.audit_chain_entry
    FOR EACH ROW EXECUTE FUNCTION platform.audit_chain_immutable();

CREATE TRIGGER audit_anchor_no_update_delete
    BEFORE UPDATE OR DELETE ON platform.audit_anchor
    FOR EACH ROW EXECUTE FUNCTION platform.audit_chain_immutable();

-- Runtime privileges: the platform role seals and reads; nobody updates or deletes evidence.
-- (Default privileges in `platform` would otherwise grant UPDATE/DELETE to sutra_platform.)
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_platform') THEN
        REVOKE UPDATE, DELETE, TRUNCATE ON platform.audit_chain_entry, platform.audit_anchor FROM sutra_platform;
        GRANT SELECT, INSERT ON platform.audit_chain_entry, platform.audit_anchor TO sutra_platform;
        REVOKE DELETE, TRUNCATE ON platform.audit_chain_head FROM sutra_platform;
        GRANT SELECT, INSERT, UPDATE ON platform.audit_chain_head TO sutra_platform;
    END IF;
END $$;
