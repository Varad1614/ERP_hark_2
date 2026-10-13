-- Batch 7 slice 7C — Domain E payroll (docs/batch7-contracts.md §3.1, §3.5; L2-E06; L3/L4
-- E-053 … E-057, E-074).
--
-- Configuration versions (components, statutory rules, rounding), salary structures and
-- effective-dated salary assignments, payroll periods, runs, per-employee results + lines
-- (immutable once finalised), adjustments, payslips, final settlements and the encashment
-- payments that record which 7B `encashment` ledger entries payroll has paid.
--
-- Money NUMERIC(14,2), rates / percentages NUMERIC(7,4), days NUMERIC(6,2) — rust_decimal in
-- Rust, strings on the wire. Engagements / staff records are referenced with FKs to the
-- foundation tables; journal entries (Domain F) are plain uuids.

-- ------------------------------------------------------------------ configuration (E-074)
CREATE TABLE people.payroll_config (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    version_no           integer      NOT NULL CHECK (version_no >= 1),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    effective_from       date         NOT NULL,
    -- Line rounding: `round2` (paise, half away from zero) or `rupee` (whole rupees).
    rounding             text         NOT NULL DEFAULT 'round2' CHECK (rounding IN ('round2', 'rupee')),
    -- Leave encashment day rate = (BASIC + DA monthly) / divisor.
    encashment_divisor   integer      NOT NULL DEFAULT 30 CHECK (encashment_divisor BETWEEN 1 AND 31),
    copied_from_id       uuid         REFERENCES people.payroll_config (id),
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'review', 'approved', 'active', 'superseded', 'archived')),
    status_reason        text,
    process_instance_id  text,
    approved_at          timestamptz,
    activated_at         timestamptz,
    superseded_at        timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX payroll_config_version_no
    ON people.payroll_config (tenant_id, institution_id, version_no);
-- One active configuration per institution.
CREATE UNIQUE INDEX payroll_config_one_active
    ON people.payroll_config (tenant_id, institution_id) WHERE status = 'active';

CREATE TABLE people.pay_component (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    config_id            uuid         NOT NULL REFERENCES people.payroll_config (id) ON DELETE CASCADE,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z][A-Z0-9_]{0,15}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 100),
    kind                 text         NOT NULL CHECK (kind IN ('earning', 'deduction', 'employer_contribution')),
    basis                text         NOT NULL CHECK (basis IN ('fixed', 'percent_of_basic', 'percent_of_gross', 'statutory')),
    -- Default monthly amount (fixed) / percentage (percent_of_*); a salary structure line wins.
    amount               numeric(14,2) CHECK (amount IS NULL OR amount >= 0),
    rate                 numeric(7,4)  CHECK (rate IS NULL OR (rate >= 0 AND rate <= 100)),
    sequence             integer      NOT NULL DEFAULT 100,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (config_id, code)
);

CREATE TABLE people.statutory_rule (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    config_id            uuid         NOT NULL REFERENCES people.payroll_config (id) ON DELETE CASCADE,
    kind                 text         NOT NULL CHECK (kind IN ('pf', 'esi', 'professional_tax', 'tds')),
    employee_rate        numeric(7,4)  CHECK (employee_rate IS NULL OR (employee_rate >= 0 AND employee_rate <= 100)),
    employer_rate        numeric(7,4)  CHECK (employer_rate IS NULL OR (employer_rate >= 0 AND employer_rate <= 100)),
    -- PF: contribution base capped at the wage ceiling.
    wage_ceiling         numeric(14,2) CHECK (wage_ceiling IS NULL OR wage_ceiling > 0),
    -- ESI: applies while monthly gross ≤ threshold.
    threshold            numeric(14,2) CHECK (threshold IS NULL OR threshold > 0),
    -- Professional tax slabs `[{"upTo": "15000.00" | null, "amount": "200.00"}]`, ascending.
    slabs                jsonb        NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(slabs) = 'array'),
    -- TDS: a configured monthly amount (else `employee_rate` % of gross).
    fixed_amount         numeric(14,2) CHECK (fixed_amount IS NULL OR fixed_amount >= 0),
    employee_component_code text      NOT NULL CHECK (employee_component_code ~ '^[A-Z][A-Z0-9_]{0,15}$'),
    employer_component_code text      CHECK (employer_component_code IS NULL OR employer_component_code ~ '^[A-Z][A-Z0-9_]{0,15}$'),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (config_id, kind)
);

-- ------------------------------------------------------------------ salary structures / assignments
CREATE TABLE people.salary_structure (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    code                 text         NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{0,31}$'),
    name                 text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    -- Pay level of `staff_engagement.grade` this structure pays (`AP-1`).
    grade                text         CHECK (grade IS NULL OR length(btrim(grade)) BETWEEN 1 AND 40),
    status               text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'retired')),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX salary_structure_code
    ON people.salary_structure (tenant_id, institution_id, code);
CREATE UNIQUE INDEX salary_structure_one_active_grade
    ON people.salary_structure (tenant_id, institution_id, grade) WHERE status = 'active' AND grade IS NOT NULL;

CREATE TABLE people.salary_structure_line (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    structure_id         uuid         NOT NULL REFERENCES people.salary_structure (id) ON DELETE CASCADE,
    component_code       text         NOT NULL CHECK (component_code ~ '^[A-Z][A-Z0-9_]{0,15}$'),
    amount               numeric(14,2) CHECK (amount IS NULL OR amount >= 0),
    rate                 numeric(7,4)  CHECK (rate IS NULL OR (rate >= 0 AND rate <= 100)),
    CHECK (amount IS NOT NULL OR rate IS NOT NULL),
    UNIQUE (structure_id, component_code)
);

CREATE TABLE people.salary_assignment (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid         NOT NULL REFERENCES people.staff_engagement (id),
    structure_id         uuid         NOT NULL REFERENCES people.salary_structure (id),
    effective_from       date         NOT NULL,
    effective_to         date,
    status               text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'ended')),
    reason               text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (effective_to IS NULL OR effective_to >= effective_from)
);
CREATE INDEX salary_assignment_engagement ON people.salary_assignment (engagement_id, effective_from);

-- ------------------------------------------------------------------ periods, runs, results (E-053)
CREATE TABLE people.payroll_period (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    year                 integer      NOT NULL CHECK (year BETWEEN 2000 AND 2100),
    month                integer      NOT NULL CHECK (month BETWEEN 1 AND 12),
    period_start         date         NOT NULL,
    period_end           date         NOT NULL,
    status               text         NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'closed')),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (period_end >= period_start),
    UNIQUE (tenant_id, institution_id, year, month)
);

CREATE TABLE people.payroll_run (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    period_id            uuid         NOT NULL REFERENCES people.payroll_period (id),
    number               text         NOT NULL CHECK (number ~ '^PR-[0-9]{6,}$'),
    kind                 text         NOT NULL CHECK (kind IN ('regular', 'off_cycle', 'final_settlement', 'retro')),
    status               text         NOT NULL DEFAULT 'created'
        CHECK (status IN ('created', 'calculating', 'calculated', 'validating', 'pending_approval', 'approved',
                          'finalized', 'posted', 'closed', 'failed', 'reprocessing')),
    status_reason        text,
    description          text         CHECK (description IS NULL OR length(description) <= 500),
    -- Configuration version the last calculation used (reproducibility).
    config_id            uuid         REFERENCES people.payroll_config (id),
    -- Off-cycle items `[{"engagementId", "componentCode", "amount", "note"}]`.
    items                jsonb        NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(items) = 'array'),
    exceptions           jsonb        NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(exceptions) = 'array'),
    headcount            integer      NOT NULL DEFAULT 0 CHECK (headcount >= 0),
    gross_earnings       numeric(14,2) NOT NULL DEFAULT 0,
    employee_deductions  numeric(14,2) NOT NULL DEFAULT 0,
    employer_contributions numeric(14,2) NOT NULL DEFAULT 0,
    net_pay              numeric(14,2) NOT NULL DEFAULT 0,
    journal_entry_id     uuid,
    journal_entry_number text,
    process_instance_id  text,
    calculated_at        timestamptz,
    approved_at          timestamptz,
    finalized_at         timestamptz,
    posted_at            timestamptz,
    closed_at            timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX payroll_run_number ON people.payroll_run (tenant_id, institution_id, number);
-- One regular run per period (a failed run is reprocessed, never duplicated).
CREATE UNIQUE INDEX payroll_run_one_regular
    ON people.payroll_run (tenant_id, period_id) WHERE kind = 'regular';

CREATE TABLE people.payroll_result (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    run_id               uuid         NOT NULL REFERENCES people.payroll_run (id),
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid         NOT NULL REFERENCES people.staff_engagement (id),
    staff_number         text         NOT NULL,
    display_name         text         NOT NULL,
    status               text         NOT NULL DEFAULT 'calculated'
        CHECK (status IN ('calculated', 'validated', 'finalized', 'superseded')),
    revision             integer      NOT NULL DEFAULT 1 CHECK (revision >= 1),
    supersedes_result_id uuid         REFERENCES people.payroll_result (id),
    -- First revision of the chain (payslips follow the chain).
    origin_result_id     uuid         NOT NULL,
    period_start         date         NOT NULL,
    period_end           date         NOT NULL,
    period_days          numeric(6,2) NOT NULL,
    employed_days        numeric(6,2) NOT NULL,
    lop_days             numeric(6,2) NOT NULL DEFAULT 0,
    payable_days         numeric(6,2) NOT NULL,
    gross_earnings       numeric(14,2) NOT NULL,
    total_deductions     numeric(14,2) NOT NULL,
    employer_contributions numeric(14,2) NOT NULL,
    net_pay              numeric(14,2) NOT NULL,
    salary_assignment_id uuid         REFERENCES people.salary_assignment (id),
    structure_id         uuid         REFERENCES people.salary_structure (id),
    config_id            uuid         REFERENCES people.payroll_config (id),
    -- Calculation snapshot: config version, rounding, rules, inputs (reproducibility, L4-E-053-BH-008).
    snapshot             jsonb        NOT NULL DEFAULT '{}'::jsonb,
    exceptions           jsonb        NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(exceptions) = 'array'),
    finalized_at         timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX payroll_result_one_current
    ON people.payroll_result (run_id, engagement_id) WHERE status <> 'superseded';
CREATE INDEX payroll_result_staff ON people.payroll_result (tenant_id, staff_id, period_start);

CREATE TABLE people.payroll_result_line (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    result_id            uuid         NOT NULL REFERENCES people.payroll_result (id) ON DELETE CASCADE,
    line_no              integer      NOT NULL CHECK (line_no >= 1),
    code                 text         NOT NULL,
    name                 text         NOT NULL,
    kind                 text         NOT NULL CHECK (kind IN ('earning', 'deduction', 'employer_contribution')),
    -- Signed: retro arrears and adjustment deltas may be negative.
    amount               numeric(14,2) NOT NULL,
    basis                text,
    source               text         NOT NULL DEFAULT 'calculation'
        CHECK (source IN ('calculation', 'encashment', 'adjustment', 'retro', 'off_cycle', 'recovery')),
    source_ref           uuid,
    UNIQUE (result_id, line_no)
);

-- A finalised result is immutable: only `finalized → superseded` (a corrective revision
-- replaces it); finalised / superseded results and their lines are never deleted or rewritten.
CREATE FUNCTION people.payroll_result_guard() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.status IN ('finalized', 'superseded') THEN
            RAISE EXCEPTION 'finalised payroll result % is immutable', OLD.id;
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.status = 'superseded'
       OR (OLD.status = 'finalized' AND (NEW.status NOT IN ('finalized', 'superseded')
           OR NEW.gross_earnings <> OLD.gross_earnings OR NEW.total_deductions <> OLD.total_deductions
           OR NEW.employer_contributions <> OLD.employer_contributions OR NEW.net_pay <> OLD.net_pay
           OR NEW.lop_days <> OLD.lop_days OR NEW.snapshot <> OLD.snapshot)) THEN
        RAISE EXCEPTION 'finalised payroll result % is immutable', OLD.id;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER payroll_result_immutable BEFORE UPDATE OR DELETE ON people.payroll_result
    FOR EACH ROW EXECUTE FUNCTION people.payroll_result_guard();

CREATE FUNCTION people.payroll_result_line_guard() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    s text;
BEGIN
    SELECT status INTO s FROM people.payroll_result WHERE id = OLD.result_id;
    IF s IN ('finalized', 'superseded') THEN
        RAISE EXCEPTION 'lines of finalised payroll result % are immutable', OLD.result_id;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER payroll_result_line_immutable BEFORE UPDATE OR DELETE ON people.payroll_result_line
    FOR EACH ROW EXECUTE FUNCTION people.payroll_result_line_guard();

-- ------------------------------------------------------------------ adjustments (E-054)
CREATE TABLE people.payroll_adjustment (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    result_id            uuid         NOT NULL REFERENCES people.payroll_result (id),
    run_id               uuid         NOT NULL REFERENCES people.payroll_run (id),
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid         NOT NULL REFERENCES people.staff_engagement (id),
    status               text         NOT NULL DEFAULT 'requested'
        CHECK (status IN ('requested', 'validated', 'pending_approval', 'approved', 'calculated', 'applied', 'closed', 'rejected')),
    status_reason        text,
    reason               text         NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 500),
    -- Requested deltas `[{"code", "kind", "name", "amount"}]` (signed).
    lines                jsonb        NOT NULL CHECK (jsonb_typeof(lines) = 'array'),
    gross_delta          numeric(14,2) NOT NULL DEFAULT 0,
    deduction_delta      numeric(14,2) NOT NULL DEFAULT 0,
    employer_delta       numeric(14,2) NOT NULL DEFAULT 0,
    net_delta            numeric(14,2) NOT NULL DEFAULT 0,
    -- The corrective result revision created on apply.
    revised_result_id    uuid         REFERENCES people.payroll_result (id),
    journal_entry_id     uuid,
    journal_entry_number text,
    process_instance_id  text,
    applied_at           timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX payroll_adjustment_result ON people.payroll_adjustment (result_id);

-- ------------------------------------------------------------------ payslips
CREATE TABLE people.payslip (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    run_id               uuid         NOT NULL REFERENCES people.payroll_run (id),
    -- The result revision the payslip shows (reissue moves it to the current revision).
    result_id            uuid         NOT NULL REFERENCES people.payroll_result (id),
    -- First result of the chain (one live payslip per employee result).
    origin_result_id     uuid         NOT NULL REFERENCES people.payroll_result (id),
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    number               text         NOT NULL CHECK (number ~ '^PS-[0-9]{6,}$'),
    status               text         NOT NULL DEFAULT 'issued' CHECK (status IN ('issued', 'reissued', 'withdrawn')),
    status_reason        text,
    issued_at            timestamptz  NOT NULL DEFAULT now(),
    reissued_at          timestamptz,
    withdrawn_at         timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX payslip_number ON people.payslip (tenant_id, institution_id, number);
CREATE UNIQUE INDEX payslip_one_live
    ON people.payslip (origin_result_id) WHERE status <> 'withdrawn';
CREATE INDEX payslip_staff ON people.payslip (tenant_id, staff_id);

-- ------------------------------------------------------------------ final settlement (E-056)
CREATE TABLE people.final_settlement (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid         NOT NULL REFERENCES people.staff_engagement (id),
    number               text         CHECK (number IS NULL OR number ~ '^FNF-[0-9]{6,}$'),
    status               text         NOT NULL DEFAULT 'initiated'
        CHECK (status IN ('initiated', 'calculated', 'pending_approval', 'approved', 'rejected', 'completed')),
    status_reason        text,
    last_working_date    date         NOT NULL,
    -- Recoveries `[{"code", "name", "amount"}]` (positive amounts deducted).
    recoveries           jsonb        NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(recoveries) = 'array'),
    notes                text,
    run_id               uuid         REFERENCES people.payroll_run (id),
    salary_amount        numeric(14,2) NOT NULL DEFAULT 0,
    encashment_amount    numeric(14,2) NOT NULL DEFAULT 0,
    encashment_days      numeric(6,2)  NOT NULL DEFAULT 0,
    recovery_amount      numeric(14,2) NOT NULL DEFAULT 0,
    net_pay              numeric(14,2) NOT NULL DEFAULT 0,
    journal_entry_id     uuid,
    journal_entry_number text,
    process_instance_id  text,
    completed_at         timestamptz,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX final_settlement_one_per_engagement ON people.final_settlement (engagement_id);
CREATE UNIQUE INDEX final_settlement_number
    ON people.final_settlement (tenant_id, institution_id, number) WHERE number IS NOT NULL;

-- ------------------------------------------------------------------ encashment payments
-- Which 7B `encashment` ledger entries payroll paid (each exactly once). Append-only.
CREATE TABLE people.encashment_payment (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    ledger_entry_id      uuid         NOT NULL REFERENCES people.leave_ledger_entry (id),
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    run_id               uuid         NOT NULL REFERENCES people.payroll_run (id),
    result_id            uuid         NOT NULL REFERENCES people.payroll_result (id),
    settlement_id        uuid         REFERENCES people.final_settlement (id),
    days                 numeric(6,2) NOT NULL CHECK (days > 0),
    amount               numeric(14,2) NOT NULL CHECK (amount >= 0),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (ledger_entry_id)
);
CREATE TRIGGER encashment_payment_append_only BEFORE UPDATE OR DELETE ON people.encashment_payment
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();
REVOKE UPDATE, DELETE, TRUNCATE ON people.encashment_payment FROM sutra_people;
