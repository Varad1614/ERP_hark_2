-- ERP_EXTENSIONS 5C — staff time tracking against research projects / sponsored grants, internal
-- projects and committees. Duration, rounding, rate, import and overlap maths ported from
-- HeyPuter/everything src/time-tracking/src/lib/{time,util,csv,importer,store}.js (MIT); see
-- THIRD_PARTY_NOTICES.md.
--
-- No research-project, grant or cost-centre master existed before this slice (Resources keeps
-- research MoUs as contracts of kind `mou_research`; Finance has no cost-centre table), so the
-- trackable project is new here and links to both by plain reference (`contract_id`,
-- `cost_centre` text) without a cross-schema foreign key (ADR-L5-0036 §3.2). Hourly cost rates
-- read the People payroll results of the same schema.
--
-- Clock times are wall-clock `timestamp`s on the entry's day in the institution's timezone;
-- `seconds` is the duration of record, never the times.
CREATE TABLE people.time_settings (
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    week_start             smallint      NOT NULL DEFAULT 1 CHECK (week_start IN (0, 1)),
    rounding_minutes       integer       NOT NULL DEFAULT 0 CHECK (rounding_minutes IN (0, 5, 6, 10, 15, 30)),
    standard_monthly_hours numeric(6,2)  NOT NULL DEFAULT 176 CHECK (standard_monthly_hours > 0 AND standard_monthly_hours <= 400),
    -- ₹/hour: a number or [{ "from": date | null, "rate": number | null }] (effective-dated).
    default_hourly_rate    jsonb         CHECK (default_hourly_rate IS NULL OR jsonb_typeof(default_hourly_rate) IN ('number', 'array')),
    timer_cap_hours        numeric(4,1)  NOT NULL DEFAULT 12 CHECK (timer_cap_hours >= 1 AND timer_cap_hours <= 24),
    version                bigint        NOT NULL DEFAULT 1,
    updated_at             timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

CREATE TABLE people.time_project (
    id                     uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    code                   text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9/_.-]{0,39}$'),
    name                   text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    kind                   text          NOT NULL CHECK (kind IN ('sponsored_research', 'consultancy', 'internal_project', 'committee', 'other')),
    -- DST, SERB (ANRF), UGC, ICSSR, AICTE, DBT, CSIR, ICMR, industry sponsor …
    funding_agency         text          CHECK (funding_agency IS NULL OR length(btrim(funding_agency)) BETWEEN 1 AND 80),
    scheme                 text          CHECK (scheme IS NULL OR length(scheme) <= 120),
    sanction_number        text          CHECK (sanction_number IS NULL OR length(sanction_number) <= 80),
    sanction_date          date,
    sanctioned_amount      numeric(14,2) CHECK (sanctioned_amount IS NULL OR sanctioned_amount >= 0),
    -- The sanction's manpower (salaries) head, against which time cost is utilised.
    manpower_budget        numeric(14,2) CHECK (manpower_budget IS NULL OR manpower_budget >= 0),
    start_date             date,
    end_date               date,
    pi_staff_id            uuid          REFERENCES people.staff_member (id),
    cost_centre            text          CHECK (cost_centre IS NULL OR length(cost_centre) <= 40),
    -- resources.contract (research MoU / sanction), plain id.
    contract_id            uuid,
    budget_kind            text          CHECK (budget_kind IS NULL OR budget_kind IN ('hours', 'amount')),
    budget_period          text          CHECK (budget_period IS NULL OR budget_period IN ('total', 'monthly')),
    budget_value           numeric(14,2) CHECK (budget_value IS NULL OR budget_value > 0),
    rounding_minutes       integer       CHECK (rounding_minutes IS NULL OR rounding_minutes IN (0, 5, 6, 10, 15, 30)),
    minimum_minutes        integer       CHECK (minimum_minutes IS NULL OR minimum_minutes IN (0, 6, 10, 15, 30, 60)),
    billable_default       boolean       NOT NULL DEFAULT true,
    open_to_all            boolean       NOT NULL DEFAULT false,
    status                 text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'closed', 'archived')),
    notes                  text          CHECK (notes IS NULL OR length(notes) <= 2000),
    created_by             text,
    version                bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz   NOT NULL DEFAULT now(),
    updated_at             timestamptz   NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR start_date IS NULL OR end_date >= start_date),
    CHECK ((budget_kind IS NULL) = (budget_period IS NULL) AND (budget_kind IS NULL) = (budget_value IS NULL)),
    CHECK (kind <> 'sponsored_research' OR (funding_agency IS NOT NULL AND start_date IS NOT NULL AND end_date IS NOT NULL)),
    UNIQUE (tenant_id, institution_id, code)
);

CREATE TABLE people.time_project_member (
    project_id             uuid          NOT NULL REFERENCES people.time_project (id) ON DELETE CASCADE,
    staff_id               uuid          NOT NULL REFERENCES people.staff_member (id),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    role                   text          NOT NULL CHECK (role IN ('pi', 'co_pi', 'member', 'jrf', 'srf', 'research_associate', 'project_assistant', 'technical_staff')),
    commitment_percent     numeric(5,2)  CHECK (commitment_percent IS NULL OR (commitment_percent >= 0 AND commitment_percent <= 100)),
    PRIMARY KEY (project_id, staff_id)
);
CREATE INDEX time_project_member_staff_idx ON people.time_project_member (tenant_id, staff_id);

CREATE TABLE people.time_entry (
    id                     uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    staff_id               uuid          NOT NULL REFERENCES people.staff_member (id),
    project_id             uuid          NOT NULL REFERENCES people.time_project (id),
    entry_date             date          NOT NULL,
    seconds                integer       NOT NULL CHECK (seconds > 0 AND seconds <= 86400),
    started_at             timestamp,
    ended_at               timestamp,
    task                   text          NOT NULL DEFAULT '' CHECK (length(task) <= 200),
    notes                  text          NOT NULL DEFAULT '' CHECK (length(notes) <= 2000),
    billable               boolean       NOT NULL DEFAULT true,
    source                 text          NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'timer', 'import')),
    created_by             text,
    version                bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz   NOT NULL DEFAULT now(),
    updated_at             timestamptz   NOT NULL DEFAULT now(),
    CHECK ((started_at IS NULL) = (ended_at IS NULL)),
    CHECK (ended_at IS NULL OR ended_at > started_at)
);
CREATE INDEX time_entry_staff_idx ON people.time_entry (tenant_id, staff_id, entry_date);
CREATE INDEX time_entry_project_idx ON people.time_entry (tenant_id, project_id, entry_date);
CREATE INDEX time_entry_institution_idx ON people.time_entry (tenant_id, institution_id, entry_date);

-- One running timer per staff member (wall-clock local start).
CREATE TABLE people.time_timer (
    staff_id               uuid          PRIMARY KEY REFERENCES people.staff_member (id),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    project_id             uuid          NOT NULL REFERENCES people.time_project (id),
    started_at             timestamp     NOT NULL,
    task                   text          NOT NULL DEFAULT '',
    notes                  text          NOT NULL DEFAULT '',
    billable               boolean       NOT NULL DEFAULT true,
    created_at             timestamptz   NOT NULL DEFAULT now()
);

-- A week of one staff member on one project: submitted → approved (locked) | rejected (open).
CREATE TABLE people.time_timesheet (
    id                     uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    staff_id               uuid          NOT NULL REFERENCES people.staff_member (id),
    project_id             uuid          NOT NULL REFERENCES people.time_project (id),
    week_start             date          NOT NULL,
    status                 text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'approved', 'rejected')),
    total_seconds          integer       NOT NULL DEFAULT 0 CHECK (total_seconds >= 0),
    billed_seconds         integer       NOT NULL DEFAULT 0 CHECK (billed_seconds >= 0),
    submitted_at           timestamptz,
    submitted_by           text,
    decided_at             timestamptz,
    decided_by             text,
    decision_note          text          CHECK (decision_note IS NULL OR length(decision_note) <= 1000),
    version                bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz   NOT NULL DEFAULT now(),
    updated_at             timestamptz   NOT NULL DEFAULT now(),
    -- Separation of duties at rest: whoever submitted never decides.
    CHECK (decided_by IS NULL OR submitted_by IS NULL OR decided_by <> submitted_by),
    UNIQUE (tenant_id, staff_id, project_id, week_start)
);
CREATE INDEX time_timesheet_institution_idx ON people.time_timesheet (tenant_id, institution_id, status, week_start);

-- A staff member's own hourly cost rate (₹; number or effective-dated periods). Without one the
-- rate comes from the last finalised payroll (gross ÷ standard monthly hours), else the default.
CREATE TABLE people.time_staff_rate (
    staff_id               uuid          PRIMARY KEY REFERENCES people.staff_member (id),
    tenant_id              uuid          NOT NULL,
    institution_id         uuid          NOT NULL,
    rate                   jsonb         NOT NULL CHECK (jsonb_typeof(rate) IN ('number', 'array')),
    version                bigint        NOT NULL DEFAULT 1,
    updated_at             timestamptz   NOT NULL DEFAULT now()
);

GRANT SELECT, INSERT, UPDATE, DELETE ON people.time_settings, people.time_project, people.time_project_member,
    people.time_entry, people.time_timer, people.time_timesheet, people.time_staff_rate TO sutra_people;
SELECT platform.apply_tenant_rls();
