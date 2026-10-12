-- Batch 2 slice 2A: institutional business policies with versioned revisions (L3-B-005).
-- A policy is identified by its stable key; each revision is immutable once effective and
-- is authoritative from its business-local `effective_from` (institution timezone) until the
-- next effective revision's date. Draft revisions never govern.
CREATE TABLE institution.institution_policy (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    institution_id    uuid        NOT NULL REFERENCES institution.institution(id),
    key               text        NOT NULL,
    title             text        NOT NULL,
    status            text        NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'effective', 'superseded')),
    -- Latest effective revision number (0 = none yet).
    revision          integer     NOT NULL DEFAULT 0,
    effective_from    date,
    version           bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT institution_policy_key_unique UNIQUE (tenant_id, institution_id, key)
);

CREATE TABLE institution.institution_policy_revision (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL REFERENCES institution.institution(id),
    policy_id       uuid        NOT NULL REFERENCES institution.institution_policy(id),
    revision        integer     NOT NULL CHECK (revision >= 1),
    status          text        NOT NULL CHECK (status IN ('draft', 'effective', 'superseded')),
    title           text        NOT NULL,
    body            text        NOT NULL,
    effective_from  date        NOT NULL,
    drafted_by      text,
    published_by    text,
    published_at    timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT institution_policy_revision_unique UNIQUE (policy_id, revision)
);
-- One open draft per policy; no two effective revisions on the same date (L4-B-005-BH-006).
CREATE UNIQUE INDEX institution_policy_revision_one_draft
    ON institution.institution_policy_revision (policy_id) WHERE status = 'draft';
CREATE UNIQUE INDEX institution_policy_revision_one_per_day
    ON institution.institution_policy_revision (policy_id, effective_from) WHERE status <> 'draft';
