-- Batch 3 slice 3C: guardians and student contacts (L2-C06, L3-C-051 … C-061).
-- Guardians are data in Batch 3 (no guardian sign-in role). `person_id` stays NULL until
-- Platform A02 (Person) lands.

CREATE TABLE student.guardian (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    person_id              uuid,
    given_name             text        NOT NULL CHECK (length(btrim(given_name)) > 0),
    family_name            text,
    email                  text,
    phone                  text,
    address                text,
    occupation             text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX guardian_institution_idx ON student.guardian (tenant_id, institution_id);

CREATE TABLE student.guardian_relationship (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    guardian_id            uuid        NOT NULL REFERENCES student.guardian (id),
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    relationship_type      text        NOT NULL CHECK (relationship_type IN ('mother', 'father',
                                           'legal_guardian', 'grandparent', 'sibling',
                                           'relative', 'sponsor', 'other')),
    is_primary             boolean     NOT NULL DEFAULT false,
    has_legal_custody      boolean     NOT NULL DEFAULT false,
    custody_basis          text,
    custody_effective_from date,
    status                 text        NOT NULL CHECK (status IN ('proposed', 'pending_verification',
                                           'verified', 'active', 'suspended', 'terminated',
                                           'historical')),
    verification_method    text,
    verification_evidence  text,
    verified_at            timestamptz,
    verified_by            text,
    process_instance_id    text,
    notes                  text,
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
-- One current relationship per guardian and student.
CREATE UNIQUE INDEX guardian_relationship_one_current
    ON student.guardian_relationship (tenant_id, guardian_id, student_id)
    WHERE status NOT IN ('terminated', 'historical');
-- At most one primary guardian per student among current relationships.
CREATE UNIQUE INDEX guardian_relationship_one_primary
    ON student.guardian_relationship (tenant_id, student_id)
    WHERE is_primary AND status NOT IN ('terminated', 'historical');
CREATE INDEX guardian_relationship_student_idx
    ON student.guardian_relationship (tenant_id, student_id);

-- Custody designations are never overwritten (L4-C-053): one row per designation / change.
CREATE TABLE student.guardian_custody_history (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    relationship_id        uuid        NOT NULL REFERENCES student.guardian_relationship (id),
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    has_legal_custody      boolean     NOT NULL,
    custody_basis          text,
    effective_from         date,
    reason                 text,
    actor                  text,
    recorded_at            timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER guardian_custody_history_immutable
    BEFORE UPDATE OR DELETE ON student.guardian_custody_history
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.guardian_consent (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    relationship_id        uuid        NOT NULL REFERENCES student.guardian_relationship (id),
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    consent_type           text        NOT NULL CHECK (consent_type IN ('data_sharing',
                                           'medical_treatment', 'field_trip', 'photography',
                                           'extracurricular', 'research', 'other')),
    scope_note             text,
    evidence_ref           text,
    granted_on             date        NOT NULL,
    valid_until            date,
    status                 text        NOT NULL CHECK (status IN ('granted', 'withdrawn', 'expired')),
    status_reason          text,
    withdrawn_at           timestamptz,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK (valid_until IS NULL OR valid_until >= granted_on)
);
CREATE UNIQUE INDEX guardian_consent_one_granted
    ON student.guardian_consent (relationship_id, consent_type) WHERE status = 'granted';

CREATE TABLE student.guardian_communication (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    relationship_id        uuid        NOT NULL REFERENCES student.guardian_relationship (id),
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    channels               text[]      NOT NULL,
    topics                 text[]      NOT NULL,
    status                 text        NOT NULL CHECK (status IN ('active', 'suspended', 'revoked', 'expired')),
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX guardian_communication_one_current
    ON student.guardian_communication (relationship_id) WHERE status IN ('active', 'suspended');

CREATE TABLE student.emergency_contact (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    relationship_id        uuid        REFERENCES student.guardian_relationship (id),
    name                   text        NOT NULL,
    relation               text        NOT NULL,
    phone                  text        NOT NULL,
    alternate_phone        text,
    priority               integer     NOT NULL CHECK (priority BETWEEN 1 AND 9),
    status                 text        NOT NULL CHECK (status IN ('active', 'suspended', 'revoked', 'expired')),
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX emergency_contact_priority
    ON student.emergency_contact (tenant_id, student_id, priority) WHERE status IN ('active', 'suspended');

CREATE TABLE student.financial_responsibility (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    relationship_id        uuid        NOT NULL REFERENCES student.guardian_relationship (id),
    responsibility_type    text        NOT NULL CHECK (responsibility_type IN ('tuition', 'hostel',
                                           'transport', 'all')),
    share_percent          numeric(5,2) NOT NULL CHECK (share_percent > 0 AND share_percent <= 100),
    effective_from         date        NOT NULL,
    effective_to           date,
    status                 text        NOT NULL CHECK (status IN ('active', 'suspended', 'revoked', 'expired')),
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_to >= effective_from)
);
CREATE INDEX financial_responsibility_student_idx
    ON student.financial_responsibility (tenant_id, student_id);

CREATE TABLE student.pickup_authorisation (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    relationship_id        uuid        REFERENCES student.guardian_relationship (id),
    person_name            text        NOT NULL,
    phone                  text,
    id_document_ref        text,
    valid_from             date        NOT NULL,
    valid_until            date,
    status                 text        NOT NULL CHECK (status IN ('active', 'suspended', 'revoked', 'expired')),
    status_reason          text,
    version                bigint      NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK (valid_until IS NULL OR valid_until >= valid_from)
);
CREATE INDEX pickup_authorisation_student_idx
    ON student.pickup_authorisation (tenant_id, student_id);

CREATE TRIGGER guardian_relationship_no_delete
    BEFORE DELETE ON student.guardian_relationship
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
CREATE TRIGGER guardian_consent_no_delete
    BEFORE DELETE ON student.guardian_consent
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();
