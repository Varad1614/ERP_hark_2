-- Batch 2 slice 2A: organisation type blueprints, organisation unit tree with history,
-- governance configuration and approved structure transformations (L2-B02; L3-B-006..012, -016).

-- Organisation type blueprint versions (PublicationStatus). Published versions are immutable;
-- a change is a successor draft (`supersedes_id`); publishing it supersedes the predecessor.
CREATE TABLE institution.org_type_blueprint (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL REFERENCES institution.institution(id),
    version_no          integer     NOT NULL CHECK (version_no >= 1),
    status              text        NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'in_review', 'published', 'superseded', 'retired')),
    supersedes_id       uuid        REFERENCES institution.org_type_blueprint(id),
    effective_from      date        NOT NULL,
    process_instance_id text,
    published_at        timestamptz,
    published_by        text,
    decision_reason     text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT org_type_blueprint_version_unique UNIQUE (institution_id, version_no)
);
CREATE UNIQUE INDEX org_type_blueprint_one_open
    ON institution.org_type_blueprint (institution_id) WHERE status IN ('draft', 'in_review');
CREATE UNIQUE INDEX org_type_blueprint_one_published
    ON institution.org_type_blueprint (institution_id) WHERE status = 'published';

CREATE TABLE institution.org_type_blueprint_unit_type (
    blueprint_id         uuid    NOT NULL REFERENCES institution.org_type_blueprint(id) ON DELETE CASCADE,
    key                  text    NOT NULL,
    label                text    NOT NULL,
    allow_root           boolean NOT NULL DEFAULT false,
    allowed_parent_types text[]  NOT NULL DEFAULT '{}',
    ordinal              integer NOT NULL,
    PRIMARY KEY (blueprint_id, key)
);

-- Organisation units: adjacency (`parent_id`) + materialised path `/<root id>/…/<id>/`.
CREATE TABLE institution.org_unit (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid        NOT NULL REFERENCES institution.institution(id),
    parent_id      uuid        REFERENCES institution.org_unit(id),
    path           text        NOT NULL,
    depth          integer     NOT NULL CHECK (depth >= 0),
    unit_type      text        NOT NULL,
    blueprint_id   uuid        NOT NULL REFERENCES institution.org_type_blueprint(id),
    code           text        NOT NULL,
    name           text        NOT NULL,
    description    text,
    status         text        NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'active', 'suspended', 'decommissioned', 'archived')),
    status_reason  text,
    version        bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT org_unit_code_unique UNIQUE (tenant_id, institution_id, code),
    CONSTRAINT org_unit_not_own_parent CHECK (parent_id IS NULL OR parent_id <> id)
);
CREATE INDEX org_unit_parent_idx ON institution.org_unit (parent_id);
CREATE INDEX org_unit_path_idx ON institution.org_unit (institution_id, path text_pattern_ops);
-- One live root per institution.
CREATE UNIQUE INDEX org_unit_one_live_root
    ON institution.org_unit (institution_id)
    WHERE parent_id IS NULL AND status IN ('draft', 'active', 'suspended');

-- Append-only representation history: every version of every unit (L4-B-008-BH-006,
-- L4-B-009-BH-003/004, L4-B-012-BH-007). Never updated.
CREATE TABLE institution.org_unit_history (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid        NOT NULL REFERENCES institution.institution(id),
    org_unit_id    uuid        NOT NULL REFERENCES institution.org_unit(id),
    unit_version   bigint      NOT NULL,
    change         text        NOT NULL,
    parent_id      uuid,
    path           text        NOT NULL,
    unit_type      text        NOT NULL,
    blueprint_id   uuid        NOT NULL,
    code           text        NOT NULL,
    name           text        NOT NULL,
    description    text,
    status         text        NOT NULL,
    reason         text,
    restructure_id uuid,
    effective_from date        NOT NULL,
    recorded_by    text,
    recorded_at    timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT org_unit_history_unique UNIQUE (org_unit_id, unit_version)
);

-- Governance configuration revisions per unit (PublicationStatus: draft → published →
-- superseded). A published revision governs from `effective_from` until the next published
-- revision's date (`effective_to` = day before), never earlier (L4-B-011-BH-002..004).
CREATE TABLE institution.governance_config (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid        NOT NULL REFERENCES institution.institution(id),
    org_unit_id    uuid        NOT NULL REFERENCES institution.org_unit(id),
    revision       integer     NOT NULL CHECK (revision >= 1),
    status         text        NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'in_review', 'published', 'superseded', 'retired')),
    supersedes_id  uuid        REFERENCES institution.governance_config(id),
    settings       jsonb       NOT NULL,
    effective_from date        NOT NULL,
    effective_to   date,
    published_at   timestamptz,
    published_by   text,
    version        bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT governance_config_revision_unique UNIQUE (org_unit_id, revision),
    CONSTRAINT governance_config_dates CHECK (effective_to IS NULL OR effective_to >= effective_from)
);
CREATE UNIQUE INDEX governance_config_one_open
    ON institution.governance_config (org_unit_id) WHERE status IN ('draft', 'in_review');
CREATE UNIQUE INDEX governance_config_one_per_day
    ON institution.governance_config (org_unit_id, effective_from) WHERE status IN ('published', 'superseded');

-- Approved structure transformations (move / merge) through the WorkflowPort.
CREATE TABLE institution.org_restructure (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL REFERENCES institution.institution(id),
    kind                text        NOT NULL CHECK (kind IN ('move', 'merge')),
    status              text        NOT NULL CHECK (status IN ('pending_approval', 'applied', 'returned', 'rejected')),
    reason              text        NOT NULL,
    effective_from      date        NOT NULL,
    merge_target_id     uuid        REFERENCES institution.org_unit(id),
    process_instance_id text,
    requested_by        text,
    decided_by          text,
    decided_at          timestamptz,
    decision_reason     text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX org_restructure_institution_idx ON institution.org_restructure (tenant_id, institution_id);

CREATE TABLE institution.org_restructure_move (
    id               uuid    PRIMARY KEY DEFAULT uuidv7(),
    restructure_id   uuid    NOT NULL REFERENCES institution.org_restructure(id),
    org_unit_id      uuid    NOT NULL REFERENCES institution.org_unit(id),
    role             text    NOT NULL CHECK (role IN ('move', 'merge_source')),
    from_parent_id   uuid,
    to_parent_id     uuid,
    baseline_version bigint  NOT NULL,
    ordinal          integer NOT NULL,
    CONSTRAINT org_restructure_move_unique UNIQUE (restructure_id, org_unit_id)
);
