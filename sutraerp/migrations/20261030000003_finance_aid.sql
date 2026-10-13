-- Batch 5 slice 5A: financial aid (L3-F-062 … F-066). An allocation is created from an approved
-- Domain C scholarship decision read through `AidDecisionSource` (no cross-schema SQL);
-- `scholarship_eligibility_id`, `scholarship_id` and `student_id` are plain uuids.
CREATE TABLE finance.aid_allocation (
    id                          uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                   uuid          NOT NULL,
    institution_id              uuid          NOT NULL,
    allocation_number           text          NOT NULL,
    scholarship_eligibility_id  uuid          NOT NULL,
    scholarship_id              uuid          NOT NULL,
    scholarship_code            text          NOT NULL,
    scholarship_name            text          NOT NULL,
    sponsor_type                text          NOT NULL,
    student_id                  uuid          NOT NULL,
    student_account_id          uuid          NOT NULL REFERENCES finance.student_account (id),
    academic_year_id            uuid,
    benefit_type                text          NOT NULL CHECK (benefit_type IN ('fixed_amount',
                                                  'fee_percentage', 'fee_waiver', 'stipend')),
    -- Amount (fixed / stipend) or percentage (fee_percentage); NULL for a full waiver.
    benefit_value               numeric(18,2) CHECK (benefit_value IS NULL OR benefit_value > 0),
    -- Ceiling of the allocation (fixed / stipend); NULL = bounded by the charges (percentage / waiver).
    allocated_amount            numeric(18,2) CHECK (allocated_amount IS NULL OR allocated_amount > 0),
    applied_amount              numeric(18,2) NOT NULL DEFAULT 0 CHECK (applied_amount >= 0),
    disbursed_amount            numeric(18,2) NOT NULL DEFAULT 0 CHECK (disbursed_amount >= 0),
    recovered_amount            numeric(18,2) NOT NULL DEFAULT 0 CHECK (recovered_amount >= 0),
    currency                    text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    valid_from                  date,
    valid_until                 date,
    renewal_count               integer       NOT NULL DEFAULT 0,
    decision_status             text          NOT NULL,
    status                      text          NOT NULL DEFAULT 'proposed' CHECK (status IN ('proposed',
                                                  'approved', 'allocated', 'disbursed', 'suspended',
                                                  'revoked', 'recovered', 'closed')),
    status_reason               text,
    process_instance_id         text,
    decided_by                  text,
    revoked_at                  timestamptz,
    version                     bigint        NOT NULL DEFAULT 1,
    created_at                  timestamptz   NOT NULL DEFAULT now(),
    updated_at                  timestamptz   NOT NULL DEFAULT now(),
    CHECK (allocated_amount IS NULL OR applied_amount + disbursed_amount <= allocated_amount)
);
-- Idempotency: one allocation per handed-off decision (contract §6.2).
CREATE UNIQUE INDEX aid_allocation_one_per_decision
    ON finance.aid_allocation (tenant_id, scholarship_eligibility_id);
CREATE UNIQUE INDEX aid_allocation_number_per_institution
    ON finance.aid_allocation (tenant_id, institution_id, allocation_number);
CREATE INDEX aid_allocation_account_idx ON finance.aid_allocation (tenant_id, student_account_id);

-- Every utilisation of an allocation: applied to a charge (charge adjustment) or paid in cash.
CREATE TABLE finance.aid_disbursement (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    aid_allocation_id    uuid          NOT NULL REFERENCES finance.aid_allocation (id),
    mode                 text          NOT NULL CHECK (mode IN ('charge_adjustment', 'cash')),
    charge_id            uuid          REFERENCES finance.charge (id),
    amount               numeric(18,2) NOT NULL CHECK (amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    posting_date         date          NOT NULL,
    journal_entry_id     uuid          NOT NULL REFERENCES finance.journal_entry (id),
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK ((mode = 'charge_adjustment') = (charge_id IS NOT NULL))
);
CREATE INDEX aid_disbursement_allocation_idx ON finance.aid_disbursement (tenant_id, aid_allocation_id);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.aid_disbursement FROM sutra_finance;
CREATE TRIGGER aid_disbursement_append_only BEFORE UPDATE OR DELETE ON finance.aid_disbursement
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- Recovery of utilised aid after revocation: a published `aid_recovery` charge.
CREATE TABLE finance.aid_recovery (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    aid_allocation_id    uuid          NOT NULL REFERENCES finance.aid_allocation (id),
    amount               numeric(18,2) NOT NULL CHECK (amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    recovery_charge_id   uuid          NOT NULL REFERENCES finance.charge (id),
    reason               text          NOT NULL,
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX aid_recovery_one_per_allocation ON finance.aid_recovery (tenant_id, aid_allocation_id);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.aid_recovery FROM sutra_finance;
CREATE TRIGGER aid_recovery_append_only BEFORE UPDATE OR DELETE ON finance.aid_recovery
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- Programme-level reconciliation runs (F-066): each run is a new fact, never rewritten.
CREATE TABLE finance.aid_programme_reconciliation (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    scholarship_id       uuid,
    academic_year_id     uuid,
    allocation_count     integer       NOT NULL,
    allocated_total      numeric(18,2) NOT NULL,
    utilised_total       numeric(18,2) NOT NULL,
    recovered_total      numeric(18,2) NOT NULL,
    ledger_total         numeric(18,2) NOT NULL,
    variance             numeric(18,2) NOT NULL,
    exceptions           jsonb         NOT NULL DEFAULT '[]'::jsonb,
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status               text          NOT NULL CHECK (status IN ('open', 'matched', 'exceptions',
                                           'resolved', 'closed')),
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX aid_programme_reconciliation_idx
    ON finance.aid_programme_reconciliation (tenant_id, institution_id, created_at DESC);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.aid_programme_reconciliation FROM sutra_finance;
CREATE TRIGGER aid_programme_reconciliation_append_only BEFORE UPDATE OR DELETE
    ON finance.aid_programme_reconciliation
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();
