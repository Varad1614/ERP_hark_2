-- ERP_EXTENSIONS 2B — service-desk rules and SLA. Logic ported from HeyPuter/everything
-- help-desk src/shared/rules.js and runTriggers (src/server/tickets.js) (MIT); see
-- THIRD_PARTY_NOTICES.md.
--
-- Tickets gain tags and the time of the first response. Triggers (conditions + actions) run on
-- tickets opened and changed; each firing is logged. Saved views are condition sets that list
-- tickets. The SLA policy gives first-response / next-reply / resolution targets per priority.
ALTER TABLE campus.service_ticket
    ADD COLUMN tags text[] NOT NULL DEFAULT '{}' CHECK (cardinality(tags) <= 30),
    ADD COLUMN first_response_at timestamptz;
CREATE INDEX service_ticket_tags_idx ON campus.service_ticket USING gin (tags);

CREATE TABLE campus.ticket_rule (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 120),
    event                text         NOT NULL DEFAULT 'any' CHECK (event IN ('created', 'updated', 'any')),
    position             integer      NOT NULL DEFAULT 0,
    active               boolean      NOT NULL DEFAULT true,
    conditions           jsonb        NOT NULL DEFAULT '{"all": [], "any": []}',
    actions              jsonb        NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(actions) = 'array'),
    version              bigint       NOT NULL DEFAULT 1,
    created_by           text,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX ticket_rule_institution_idx ON campus.ticket_rule (tenant_id, institution_id, position);

-- One row per trigger that fired on a ticket. Append-only.
CREATE TABLE campus.ticket_rule_run (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    ticket_id            uuid         NOT NULL REFERENCES campus.service_ticket (id),
    rule_id              uuid,
    rule_title           text         NOT NULL,
    event                text         NOT NULL CHECK (event IN ('created', 'updated')),
    changes              jsonb        NOT NULL DEFAULT '{}',
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX ticket_rule_run_ticket_idx ON campus.ticket_rule_run (tenant_id, ticket_id, created_at);
REVOKE UPDATE, DELETE, TRUNCATE ON campus.ticket_rule_run FROM sutra_campus;
CREATE TRIGGER ticket_rule_run_append_only BEFORE UPDATE OR DELETE ON campus.ticket_rule_run
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

CREATE TABLE campus.ticket_view (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    title                text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 120),
    conditions           jsonb        NOT NULL DEFAULT '{"all": [], "any": []}',
    sort                 text         NOT NULL DEFAULT 'updated_desc'
                                      CHECK (sort IN ('updated_desc', 'created_asc', 'created_desc', 'sla_asc')),
    position             integer      NOT NULL DEFAULT 0,
    created_by           text,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX ticket_view_institution_idx ON campus.ticket_view (tenant_id, institution_id, position);

CREATE TABLE campus.ticket_sla_policy (
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    enabled              boolean      NOT NULL DEFAULT true,
    -- { "<priority>": { "first": <min>, "next": <min>, "resolve": <min> } }
    targets              jsonb        NOT NULL CHECK (jsonb_typeof(targets) = 'object'),
    version              bigint       NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON campus.ticket_rule, campus.ticket_view,
    campus.ticket_sla_policy TO sutra_campus;
GRANT SELECT, INSERT ON campus.ticket_rule_run TO sutra_campus;
-- Forced row-level security on the new tenant tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
