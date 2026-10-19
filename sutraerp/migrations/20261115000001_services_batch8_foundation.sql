-- Batch 8 foundation — Domain H (Institutional services & intelligence) shared tables
-- (docs/batch8-contracts.md §3). Slice 8C adds its dashboard / regulatory tables in 20261115000050-069
-- (services.<what>). Figures of other domains come only through OperationalMetricsPort (no
-- cross-schema reads).

-- Append-only guard reused by every services history / ledger table (slices reuse it too).
CREATE FUNCTION services.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of services.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

CREATE TABLE services.number_sequence (
    tenant_id        uuid    NOT NULL,
    institution_id   uuid    NOT NULL,
    kind             text    NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{1,47}$'),
    next_value       bigint  NOT NULL CHECK (next_value >= 1),
    PRIMARY KEY (tenant_id, institution_id, kind)
);

CREATE TABLE services.status_history (
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
CREATE INDEX services_status_history_aggregate_idx
    ON services.status_history (tenant_id, aggregate_type, aggregate_id, created_at);
CREATE TRIGGER services_status_history_append_only BEFORE UPDATE OR DELETE ON services.status_history
    FOR EACH ROW EXECUTE FUNCTION services.forbid_history_change();

GRANT USAGE ON SCHEMA services TO sutra_services;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA services TO sutra_services;
REVOKE UPDATE, DELETE, TRUNCATE ON services.status_history FROM sutra_services;
GRANT INSERT ON platform.outbox, platform.audit_log TO sutra_services;
