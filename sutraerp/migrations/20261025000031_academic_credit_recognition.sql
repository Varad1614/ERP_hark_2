-- 4C — Certificates, credit mapping and recognition (L3-D-039, 043, 046, 049).
--
-- Certificate verification (submitted → under_verification → accepted | rejected | flagged),
-- versioned credit mapping rules (one effective rule per opportunity), credit recognition
-- (requested → under_review → approved → posted | rejected) whose posting goes through
-- `CreditPostingPort` into the Domain C ledger (the receipt is recorded in an append-only
-- posting history), and external record reconciliation runs.

CREATE TABLE academic.certificate_verification (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    enrolment_id        uuid        NOT NULL REFERENCES academic.flexible_enrolment (id),
    provider_id         uuid        REFERENCES academic.learning_provider (id),
    student_id          uuid        NOT NULL,
    certificate_number  text        NOT NULL CHECK (length(btrim(certificate_number)) BETWEEN 1 AND 120),
    certificate_url     text,
    issued_on           date,
    learner_name        text,
    status              text        NOT NULL DEFAULT 'submitted' CHECK (status IN (
        'submitted', 'under_verification', 'accepted', 'rejected', 'flagged')),
    -- {authenticityConfirmed, identityMatched, providerAccredited, fraudIndicators[]}
    checks              jsonb       NOT NULL DEFAULT '{}'::jsonb,
    decision_reason     text,
    decided_by          text,
    decided_at          timestamptz,
    submitted_by        text,
    process_instance_id text,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
-- The same certificate is evidence once per enrolment while it is live.
CREATE UNIQUE INDEX certificate_verification_live ON academic.certificate_verification (enrolment_id, lower(certificate_number))
    WHERE status <> 'rejected';
CREATE INDEX certificate_verification_scope ON academic.certificate_verification (tenant_id, institution_id, status);
CREATE TRIGGER certificate_verification_no_delete BEFORE DELETE ON academic.certificate_verification
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.credit_mapping_rule (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    institution_id          uuid        NOT NULL,
    opportunity_id          uuid        NOT NULL REFERENCES academic.learning_opportunity (id),
    -- Section B definitions (plain ids, validated through the reference port).
    curriculum_version_id   uuid,
    curriculum_component_id uuid,
    equivalency             text        NOT NULL DEFAULT 'elective_credit' CHECK (equivalency IN (
        'component_equivalent', 'elective_credit', 'additional_credit')),
    credits                 numeric(7,2) NOT NULL CHECK (credits > 0 AND credits <= 100),
    version_no              integer     NOT NULL CHECK (version_no >= 1),
    effective_from          date        NOT NULL,
    effective_to            date,
    status                  text        NOT NULL DEFAULT 'draft' CHECK (status IN (
        'draft', 'in_review', 'effective', 'superseded', 'retired')),
    status_reason           text,
    status_changed_at       timestamptz NOT NULL DEFAULT now(),
    supersedes_id           uuid        REFERENCES academic.credit_mapping_rule (id),
    process_instance_id     text,
    version                 bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_from <= effective_to),
    CHECK (equivalency <> 'component_equivalent' OR curriculum_component_id IS NOT NULL)
);
CREATE UNIQUE INDEX credit_mapping_rule_version ON academic.credit_mapping_rule (opportunity_id, version_no);
CREATE UNIQUE INDEX credit_mapping_rule_one_effective ON academic.credit_mapping_rule (opportunity_id)
    WHERE status = 'effective';
CREATE TRIGGER credit_mapping_rule_no_delete BEFORE DELETE ON academic.credit_mapping_rule
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.credit_recognition (
    id                          uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                   uuid        NOT NULL,
    institution_id              uuid        NOT NULL,
    flexible_enrolment_id       uuid        NOT NULL REFERENCES academic.flexible_enrolment (id),
    certificate_verification_id uuid        NOT NULL REFERENCES academic.certificate_verification (id),
    mapping_rule_id             uuid        NOT NULL REFERENCES academic.credit_mapping_rule (id),
    opportunity_id              uuid        NOT NULL REFERENCES academic.learning_opportunity (id),
    student_id                  uuid        NOT NULL,
    student_subject_ref         text,
    academic_term_id            uuid,
    curriculum_component_id     uuid,
    -- Mapped credits requested; `credits` = approved (≤ requested; partial approval).
    requested_credits           numeric(7,2) NOT NULL CHECK (requested_credits > 0),
    credits                     numeric(7,2) NOT NULL CHECK (credits > 0),
    status                      text        NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'under_review', 'approved', 'posted', 'rejected')),
    decision_reason             text,
    decided_by                  text,
    process_instance_id         text,
    ledger_entry_id             uuid,
    posted_at                   timestamptz,
    version                     bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                  timestamptz NOT NULL DEFAULT now(),
    updated_at                  timestamptz NOT NULL DEFAULT now(),
    CHECK (credits <= requested_credits),
    CHECK (status <> 'posted' OR ledger_entry_id IS NOT NULL)
);
-- One live recognition per enrolment (L4-D-039-BH-009).
CREATE UNIQUE INDEX credit_recognition_one_live ON academic.credit_recognition (flexible_enrolment_id)
    WHERE status <> 'rejected';
CREATE INDEX credit_recognition_scope ON academic.credit_recognition (tenant_id, institution_id, status);
CREATE TRIGGER credit_recognition_no_delete BEFORE DELETE ON academic.credit_recognition
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- Append-only receipts of postings into the Domain C ledger.
CREATE TABLE academic.credit_recognition_posting (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    recognition_id  uuid        NOT NULL REFERENCES academic.credit_recognition (id),
    ledger_entry_id uuid        NOT NULL,
    already_posted  boolean     NOT NULL,
    credits         numeric(7,2) NOT NULL,
    posted_by       text,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX credit_recognition_posting_recognition ON academic.credit_recognition_posting (recognition_id);
CREATE TRIGGER credit_recognition_posting_append_only BEFORE UPDATE OR DELETE ON academic.credit_recognition_posting
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

-- External learning record reconciliation (L3-D-049): institutional records vs provider evidence.
CREATE TABLE academic.reconciliation_run (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    institution_id    uuid        NOT NULL,
    opportunity_id    uuid        NOT NULL REFERENCES academic.learning_opportunity (id),
    provider_id       uuid        REFERENCES academic.learning_provider (id),
    -- Provider evidence as received (never overwritten).
    provider_records  jsonb       NOT NULL,
    -- Institutional snapshot at comparison time.
    institutional_records jsonb   NOT NULL,
    discrepancies     jsonb       NOT NULL DEFAULT '[]'::jsonb,
    status            text        NOT NULL DEFAULT 'open' CHECK (status IN (
        'open', 'discrepancies', 'reconciled', 'escalated')),
    resolution        text,
    resolution_notes  text,
    exception_case_id uuid        REFERENCES academic.flexible_exception_case (id),
    run_by            text,
    resolved_by       text,
    resolved_at       timestamptz,
    version           bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX reconciliation_run_scope ON academic.reconciliation_run (tenant_id, institution_id, status);
CREATE TRIGGER reconciliation_run_no_delete BEFORE DELETE ON academic.reconciliation_run
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();
