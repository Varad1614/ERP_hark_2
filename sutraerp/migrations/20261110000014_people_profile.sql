-- Batch 7 slice 7A — employee profile depth (L2-E07): qualifications (L3-E-060), profile documents
-- (L3-E-064), governed corrections with history (L3-E-065) and workforce → profile
-- synchronisation records (L3-E-066). Corrections and synchronisation records are append-only.

CREATE TABLE people.staff_qualification (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    qualification_type        text         NOT NULL CHECK (qualification_type IN ('doctorate', 'postgraduate', 'graduate', 'diploma', 'certificate', 'other')),
    title                     text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    awarding_body             text         NOT NULL CHECK (length(btrim(awarding_body)) BETWEEN 1 AND 200),
    year_of_completion        integer      CHECK (year_of_completion IS NULL OR year_of_completion BETWEEN 1940 AND 2200),
    grade                     text,
    status                    text         NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'verified', 'rejected', 'expired')),
    status_reason             text,
    verified_by               text,
    verified_at               timestamptz,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX staff_qualification_staff_idx ON people.staff_qualification (tenant_id, staff_id);
CREATE TRIGGER staff_qualification_no_delete BEFORE DELETE ON people.staff_qualification
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

CREATE TABLE people.staff_document (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    document_type             text         NOT NULL CHECK (document_type IN ('identity', 'address', 'education', 'employment', 'medical', 'other')),
    title                     text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    -- Document number or storage reference (never the content).
    reference                 text         NOT NULL CHECK (length(btrim(reference)) BETWEEN 1 AND 200),
    valid_until               date,
    status                    text         NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'verified', 'rejected', 'expired')),
    status_reason             text,
    verified_by               text,
    verified_at               timestamptz,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX staff_document_staff_idx ON people.staff_document (tenant_id, staff_id);
CREATE TRIGGER staff_document_no_delete BEFORE DELETE ON people.staff_document
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- Governed corrections: one row per corrected field with its value before and after.
CREATE TABLE people.profile_correction (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    field                     text         NOT NULL,
    before_value              text,
    after_value               text,
    reason                    text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    staff_version             bigint       NOT NULL,
    actor                     text,
    correlation_id            text,
    created_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX profile_correction_staff_idx ON people.profile_correction (tenant_id, staff_id, created_at);
REVOKE UPDATE, DELETE, TRUNCATE ON people.profile_correction FROM sutra_people;
CREATE TRIGGER profile_correction_append_only BEFORE UPDATE OR DELETE ON people.profile_correction
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- Workforce change → profile synchronisation (which fields followed an engagement change).
CREATE TABLE people.profile_sync (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id             uuid         NOT NULL REFERENCES people.staff_engagement (id),
    cause                     text         NOT NULL CHECK (cause IN ('modification', 'transfer', 'probation', 'activation')),
    fields                    text[]       NOT NULL,
    before_values             jsonb        NOT NULL DEFAULT '{}'::jsonb,
    after_values              jsonb        NOT NULL DEFAULT '{}'::jsonb,
    actor                     text,
    created_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX profile_sync_staff_idx ON people.profile_sync (tenant_id, staff_id, created_at);
REVOKE UPDATE, DELETE, TRUNCATE ON people.profile_sync FROM sutra_people;
CREATE TRIGGER profile_sync_append_only BEFORE UPDATE OR DELETE ON people.profile_sync
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();
