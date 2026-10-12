-- Batch 2 slice 2A: institution profile (L2-B01 §7 mandatory master information), profile
-- revision history (L3-B-004) and activation requests (L2-B01 + activation-authority amendment).
ALTER TABLE institution.institution
    ADD COLUMN short_name            text,
    -- school | college | university | autonomous | affiliated | other (2B reads `school`).
    ADD COLUMN institution_type      text CONSTRAINT institution_type_check CHECK (institution_type IN (
        'school', 'college', 'university', 'autonomous', 'affiliated', 'other')),
    ADD COLUMN affiliated_university text,
    ADD COLUMN regulatory_bodies     text[] NOT NULL DEFAULT '{}',
    ADD COLUMN registration_number   text,
    ADD COLUMN address               text,
    ADD COLUMN contact_phone         text,
    ADD COLUMN official_email        text,
    ADD COLUMN website               text,
    ADD COLUMN academic_pattern      text CONSTRAINT institution_academic_pattern_check CHECK (academic_pattern IN (
        'annual', 'semester', 'trimester', 'term')),
    ADD COLUMN working_days          text[] NOT NULL DEFAULT '{}',
    ADD COLUMN default_language      text,
    ADD COLUMN administrator_email   text,
    ADD COLUMN profile_revision      integer NOT NULL DEFAULT 0;

-- Every applied profile revision keeps the previous and the new values (L4-B-004-BH-005).
CREATE TABLE institution.institution_profile_revision (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL REFERENCES institution.institution(id),
    revision        integer     NOT NULL CHECK (revision >= 1),
    changed_fields  text[]      NOT NULL,
    previous_values jsonb       NOT NULL,
    new_values      jsonb       NOT NULL,
    effective_from  date        NOT NULL,
    revised_by      text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT institution_profile_revision_unique UNIQUE (institution_id, revision)
);
CREATE INDEX institution_profile_revision_tenant_idx
    ON institution.institution_profile_revision (tenant_id, institution_id);

-- One row per activation request cycle (configured → pending_approval → decision).
CREATE TABLE institution.activation_request (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL REFERENCES institution.institution(id),
    status              text        NOT NULL CHECK (status IN ('pending', 'approved', 'returned', 'rejected')),
    submitted_version   bigint      NOT NULL,
    process_instance_id text,
    readiness           jsonb       NOT NULL,
    submitted_by        text,
    submitted_at        timestamptz NOT NULL DEFAULT now(),
    decided_by          text,
    decided_at          timestamptz,
    decision_reason     text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX activation_request_institution_idx
    ON institution.activation_request (tenant_id, institution_id, submitted_at DESC);
-- At most one open request per institution.
CREATE UNIQUE INDEX activation_request_one_pending
    ON institution.activation_request (institution_id) WHERE status = 'pending';
