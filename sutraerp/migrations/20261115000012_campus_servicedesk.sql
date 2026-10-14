-- Batch 8 slice 8A — service desk escalations (L2-G01; L3-G-041, 042). Tickets themselves are the
-- foundation table campus.service_ticket (opened via shared::tickets::open_ticket, status via
-- change_ticket_status). Append-only.

CREATE TABLE campus.ticket_escalation (
    id               uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid         NOT NULL,
    institution_id   uuid         NOT NULL,
    ticket_id        uuid         NOT NULL REFERENCES campus.service_ticket (id),
    level            integer      NOT NULL CHECK (level >= 1),
    -- `sla_breach` (SLA check) or `manual`.
    kind             text         NOT NULL CHECK (kind IN ('sla_breach', 'manual')),
    reason           text,
    actor            text,
    created_at       timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX ticket_escalation_ticket ON campus.ticket_escalation (tenant_id, ticket_id, level);
REVOKE UPDATE, DELETE, TRUNCATE ON campus.ticket_escalation FROM sutra_campus;
CREATE TRIGGER ticket_escalation_append_only BEFORE UPDATE OR DELETE ON campus.ticket_escalation
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
