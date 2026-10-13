-- Batch 5 slice 5A (Domain F, Finance): fee structures, charge revisions, instalment plans and
-- charge disputes (L3-F-001 … F-006). Charges themselves live in the foundation tables
-- (`finance.charge`, `finance.charge_line`); every status change goes through
-- `shared::lifecycle::change_charge_status` and every amount change through
-- `shared::charge::apply_adjustment` (docs/batch5-contracts.md §3.3).

-- ------------------------------------------------------------------ fee structures (versioned)
CREATE TABLE finance.fee_structure (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    code                 text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    version_no           integer       NOT NULL CHECK (version_no > 0),
    name                 text          NOT NULL CHECK (length(btrim(name)) > 0),
    academic_year_id     uuid,
    academic_term_id     uuid,
    -- Optional programme the structure applies to (Domain B/C reference, plain uuid).
    programme_id         uuid,
    due_date             date,
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    total_amount         numeric(18,2) NOT NULL CHECK (total_amount > 0),
    status               text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'in_review',
                                           'approved', 'published', 'superseded', 'retired')),
    status_reason        text,
    process_instance_id  text,
    approved_by          text,
    published_at         timestamptz,
    superseded_by_id     uuid          REFERENCES finance.fee_structure (id),
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX fee_structure_code_version
    ON finance.fee_structure (tenant_id, institution_id, code, version_no);
-- At most one published version per code.
CREATE UNIQUE INDEX fee_structure_one_published
    ON finance.fee_structure (tenant_id, institution_id, code) WHERE status = 'published';

CREATE TABLE finance.fee_structure_line (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    fee_structure_id     uuid          NOT NULL REFERENCES finance.fee_structure (id),
    line_no              integer       NOT NULL CHECK (line_no > 0),
    fee_head_id          uuid          NOT NULL REFERENCES finance.fee_head (id),
    description          text          NOT NULL CHECK (length(btrim(description)) > 0),
    amount               numeric(18,2) NOT NULL CHECK (amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    UNIQUE (fee_structure_id, line_no)
);

-- ------------------------------------------------------------------ governed charge revisions (F-002)
-- Append-only evidence of every revision of a charge (due date / description changes and
-- amount revisions, which are realised as posted adjustments).
CREATE TABLE finance.charge_revision (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    charge_id            uuid          NOT NULL REFERENCES finance.charge (id),
    revision_no          integer       NOT NULL CHECK (revision_no > 0),
    reason               text          NOT NULL CHECK (length(btrim(reason)) > 0),
    changed_fields       text[]        NOT NULL,
    before_state         jsonb         NOT NULL,
    after_state          jsonb         NOT NULL,
    -- Amount revisions post a credit / debit note (5A adjustment table, same slice).
    adjustment_id        uuid,
    process_instance_id  text,
    actor                text,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (charge_id, revision_no)
);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.charge_revision FROM sutra_finance;
CREATE TRIGGER charge_revision_append_only BEFORE UPDATE OR DELETE ON finance.charge_revision
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- ------------------------------------------------------------------ instalments (F-005)
CREATE TABLE finance.instalment_plan (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    charge_id            uuid          NOT NULL REFERENCES finance.charge (id),
    student_account_id   uuid          NOT NULL REFERENCES finance.student_account (id),
    instalment_count     integer       NOT NULL CHECK (instalment_count BETWEEN 1 AND 24),
    total_amount         numeric(18,2) NOT NULL CHECK (total_amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status               text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'replaced', 'cancelled')),
    reason               text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX instalment_plan_one_active
    ON finance.instalment_plan (tenant_id, charge_id) WHERE status = 'active';

CREATE TABLE finance.instalment (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    instalment_plan_id   uuid          NOT NULL REFERENCES finance.instalment_plan (id),
    charge_id            uuid          NOT NULL REFERENCES finance.charge (id),
    sequence             integer       NOT NULL CHECK (sequence > 0),
    due_date             date          NOT NULL,
    amount               numeric(18,2) NOT NULL CHECK (amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status               text          NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'due',
                                           'overdue', 'paid', 'waived', 'cancelled')),
    due_notified_at      timestamptz,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (instalment_plan_id, sequence)
);
CREATE INDEX instalment_charge_idx ON finance.instalment (tenant_id, charge_id, sequence);

-- ------------------------------------------------------------------ disputes (F-004)
CREATE TABLE finance.charge_dispute (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    charge_id            uuid          NOT NULL REFERENCES finance.charge (id),
    student_account_id   uuid          NOT NULL REFERENCES finance.student_account (id),
    raised_by            text,
    raised_by_student    boolean       NOT NULL DEFAULT false,
    reason               text          NOT NULL CHECK (length(btrim(reason)) > 0),
    disputed_amount      numeric(18,2) CHECK (disputed_amount IS NULL OR disputed_amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    -- Charge status before the dispute (restored when it is rejected / withdrawn).
    prior_charge_status  text          NOT NULL,
    status               text          NOT NULL DEFAULT 'raised' CHECK (status IN ('raised',
                                           'under_review', 'upheld', 'rejected', 'withdrawn')),
    resolution           text          CHECK (resolution IS NULL OR resolution IN ('none', 'credit', 'waive')),
    resolution_note      text,
    adjustment_id        uuid,
    decided_by           text,
    decided_at           timestamptz,
    process_instance_id  text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX charge_dispute_one_open
    ON finance.charge_dispute (tenant_id, charge_id) WHERE status IN ('raised', 'under_review');
