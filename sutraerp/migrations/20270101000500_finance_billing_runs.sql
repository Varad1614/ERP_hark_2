-- ERP_EXTENSIONS 1E — periodic billing runs. Logic ported from HeyPuter/everything childcare
-- core/billing.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- A billing plan bills a fee head at a frequency; students are enrolled on plans (optionally at
-- their own amount, from / until a date, or on hold). A run for a period raises one charge per
-- student (`source_type` 'billing_run', `source_id` = the run item) with a line per plan, at most
-- once per student, frequency and period while that charge stands. A schedule per frequency lets
-- the server run it on a day of each period.

DO $$
DECLARE
    c text;
BEGIN
    FOR c IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'finance.charge'::regclass AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%source_type%'
    LOOP
        EXECUTE format('ALTER TABLE finance.charge DROP CONSTRAINT %I', c);
    END LOOP;
END $$;

ALTER TABLE finance.charge ADD CONSTRAINT charge_source_type_check CHECK (source_type IN (
    'fee_structure', 'manual', 'admission', 'debit_note', 'aid_recovery', 'fine', 'exam_fee',
    'campus_charge', 'billing_run'));

CREATE UNIQUE INDEX charge_billing_run_source ON finance.charge (tenant_id, source_id)
    WHERE source_type = 'billing_run';

CREATE TABLE finance.billing_plan (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    code                 text           NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name                 text           NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
    description          text,
    fee_head_id          uuid           NOT NULL REFERENCES finance.fee_head (id),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    frequency            text           NOT NULL CHECK (frequency IN ('weekly', 'monthly', 'quarterly', 'half_yearly', 'annual')),
    proration            text           NOT NULL DEFAULT 'none' CHECK (proration IN ('none', 'daily')),
    status               text           NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'retired')),
    version              integer        NOT NULL DEFAULT 1,
    created_by           text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, code)
);

CREATE TABLE finance.billing_plan_enrolment (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    plan_id              uuid           NOT NULL REFERENCES finance.billing_plan (id),
    student_id           uuid           NOT NULL,
    amount               numeric(18,2)  CHECK (amount >= 0),
    start_date           date,
    end_date             date,
    on_hold              boolean        NOT NULL DEFAULT false,
    created_by           text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR start_date IS NULL OR end_date >= start_date),
    UNIQUE (tenant_id, plan_id, student_id)
);
CREATE INDEX billing_plan_enrolment_institution_idx ON finance.billing_plan_enrolment (tenant_id, institution_id);

CREATE TABLE finance.billing_run (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    frequency            text           NOT NULL,
    period_key           text           NOT NULL,
    period_start         date           NOT NULL,
    period_end           date           NOT NULL,
    period_label         text           NOT NULL,
    run_date             date           NOT NULL,
    due_days             integer        NOT NULL CHECK (due_days BETWEEN 0 AND 365),
    trigger              text           NOT NULL CHECK (trigger IN ('manual', 'schedule')),
    billed_count         integer        NOT NULL DEFAULT 0,
    skipped_count        integer        NOT NULL DEFAULT 0,
    failed_count         integer        NOT NULL DEFAULT 0,
    total                numeric(18,2)  NOT NULL DEFAULT 0,
    created_by           text,
    created_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE INDEX billing_run_institution_idx ON finance.billing_run (tenant_id, institution_id, created_at DESC);

-- One per student billed by a run; `billed` rows are unique per student, frequency and period
-- (a cancelled charge's row turns `void`, so the period can be billed again).
CREATE TABLE finance.billing_run_item (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    run_id               uuid           NOT NULL REFERENCES finance.billing_run (id),
    student_id           uuid           NOT NULL,
    frequency            text           NOT NULL,
    period_key           text           NOT NULL,
    charge_id            uuid           REFERENCES finance.charge (id),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    lines                jsonb          NOT NULL DEFAULT '[]',
    status               text           NOT NULL DEFAULT 'billed' CHECK (status IN ('billed', 'void')),
    created_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX billing_run_item_once ON finance.billing_run_item (tenant_id, institution_id, student_id, frequency, period_key)
    WHERE status = 'billed';
CREATE INDEX billing_run_item_run_idx ON finance.billing_run_item (tenant_id, run_id);

CREATE TABLE finance.billing_schedule (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    frequency            text           NOT NULL CHECK (frequency IN ('weekly', 'monthly', 'quarterly', 'half_yearly', 'annual')),
    enabled              boolean        NOT NULL DEFAULT false,
    run_day              integer        NOT NULL DEFAULT 1 CHECK (run_day BETWEEN 1 AND 31),
    due_days             integer        NOT NULL DEFAULT 7 CHECK (due_days BETWEEN 0 AND 365),
    last_period_key      text,
    last_run_id          uuid           REFERENCES finance.billing_run (id),
    updated_by           text,
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, frequency)
);
CREATE INDEX billing_schedule_enabled_idx ON finance.billing_schedule (enabled) WHERE enabled;

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.billing_plan, finance.billing_plan_enrolment,
    finance.billing_run, finance.billing_run_item, finance.billing_schedule TO sutra_finance;

-- Forced row-level security on the new tenant tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
