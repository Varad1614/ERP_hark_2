-- Batch 7 slice 7B — leave policy, leave types, leave requests (+ month segments), accrual and
-- year-end runs, governed adjustments, encashment decisions, leave exceptions, leave cases and
-- long leave (docs/batch7-contracts.md §3; L2-E05, L2-E01 long leave, L2-E04 leave case).
--
-- Owned by 7B. Leave days are NUMERIC(6,2) (rust_decimal in Rust, strings on the wire). The leave
-- ledger itself is the foundation table `people.leave_ledger_entry`, written ONLY through
-- `shared::leave::{post_entry, reverse_entry}`; attendance days ONLY through
-- `shared::attendance::record_day`; engagement status (active <-> long_leave) ONLY through
-- `shared::lifecycle::change_engagement_status`. No foreign keys into other slices' tables.

-- ------------------------------------------------------------------ leave policy versions (E-044)
CREATE TABLE people.leave_policy (
    id                    uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    name                  text         NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    version_no            integer      NOT NULL CHECK (version_no >= 1),
    effective_from        date         NOT NULL,
    -- ISO weekdays that are weekly offs (1 = Monday … 7 = Sunday).
    weekly_off_days       smallint[]   NOT NULL DEFAULT '{7}',
    holidays              date[]       NOT NULL DEFAULT '{}',
    status                text         NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'under_review',
                                           'approved', 'scheduled', 'active', 'superseded', 'archived')),
    status_reason         text,
    -- Rule validation findings of the last submission (L4-E-044-BH-002/003).
    validation            jsonb        NOT NULL DEFAULT '[]'::jsonb,
    supersedes_policy_id  uuid         REFERENCES people.leave_policy (id),
    process_instance_id   text,
    approved_at           timestamptz,
    activated_at          timestamptz,
    created_by            text,
    version               bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz  NOT NULL DEFAULT now(),
    updated_at            timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, version_no)
);
-- One active policy per institution.
CREATE UNIQUE INDEX leave_policy_one_active
    ON people.leave_policy (tenant_id, institution_id) WHERE status = 'active';

CREATE TABLE people.leave_type (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    policy_id             uuid          NOT NULL REFERENCES people.leave_policy (id),
    code                  text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{0,15}$'),
    name                  text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
    annual_entitlement    numeric(6,2)  NOT NULL DEFAULT 0 CHECK (annual_entitlement >= 0),
    accrual_frequency     text          NOT NULL DEFAULT 'yearly' CHECK (accrual_frequency IN ('monthly', 'yearly', 'none')),
    carry_forward_cap     numeric(6,2)  NOT NULL DEFAULT 0 CHECK (carry_forward_cap >= 0),
    encashment_cap        numeric(6,2)  NOT NULL DEFAULT 0 CHECK (encashment_cap >= 0),
    is_paid               boolean       NOT NULL DEFAULT true,
    requires_balance      boolean       NOT NULL DEFAULT true,
    half_day_allowed      boolean       NOT NULL DEFAULT true,
    applicable_categories text[]        NOT NULL DEFAULT '{teaching,non_teaching,administrative}',
    version               bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (policy_id, code)
);
CREATE INDEX leave_type_institution_idx ON people.leave_type (tenant_id, institution_id);

-- ------------------------------------------------------------------ leave requests (E-045/047/048)
CREATE TABLE people.leave_request (
    id                   uuid          PRIMARY KEY,
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    number               text          NOT NULL,
    staff_id             uuid          NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid          NOT NULL REFERENCES people.staff_engagement (id),
    policy_id            uuid          NOT NULL REFERENCES people.leave_policy (id),
    leave_type_id        uuid          NOT NULL REFERENCES people.leave_type (id),
    leave_type_code      text          NOT NULL,
    is_paid              boolean       NOT NULL,
    start_date           date          NOT NULL,
    end_date             date          NOT NULL,
    half_day_start       boolean       NOT NULL DEFAULT false,
    half_day_end         boolean       NOT NULL DEFAULT false,
    days                 numeric(6,2)  NOT NULL CHECK (days > 0),
    leave_year           integer       NOT NULL,
    reason               text,
    status               text          NOT NULL CHECK (status IN ('draft', 'submitted', 'validated',
                                           'pending_approval', 'approved', 'rejected', 'reserved', 'consumed',
                                           'cancelled', 'closed')),
    status_reason        text,
    -- Reporting manager the request was routed to (EngagementSummary.reports_to_staff_id at submission).
    approver_staff_id    uuid,
    decided_by           text,
    decided_at           timestamptz,
    process_instance_id  text,
    requested_by         text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (end_date >= start_date),
    UNIQUE (tenant_id, institution_id, number)
);
CREATE INDEX leave_request_staff_idx ON people.leave_request (tenant_id, staff_id, start_date);
CREATE INDEX leave_request_institution_idx ON people.leave_request (tenant_id, institution_id, status);
CREATE TRIGGER leave_request_no_delete BEFORE DELETE ON people.leave_request
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- One row per calendar month a request touches (consumption posts one ledger entry per row,
-- posting_key consume:<YYYY-MM>, effective_on = first_day; contract §7).
CREATE TABLE people.leave_request_segment (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    leave_request_id   uuid          NOT NULL REFERENCES people.leave_request (id),
    month              date          NOT NULL CHECK (extract(day FROM month) = 1),
    first_day          date          NOT NULL,
    last_day           date          NOT NULL,
    days               numeric(6,2)  NOT NULL CHECK (days > 0),
    consumed_entry_id  uuid          REFERENCES people.leave_ledger_entry (id),
    UNIQUE (leave_request_id, month)
);

-- ------------------------------------------------------------------ accrual + year-end runs (E-046/050)
CREATE TABLE people.leave_accrual_run (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    policy_id       uuid         NOT NULL REFERENCES people.leave_policy (id),
    -- `YYYY` (opening entitlements of yearly types) or `YYYY-MM` (monthly accrual).
    period          text         NOT NULL CHECK (period ~ '^[0-9]{4}(-[0-9]{2})?$'),
    leave_year      integer      NOT NULL,
    status          text         NOT NULL DEFAULT 'completed' CHECK (status IN ('completed')),
    staff_count     integer      NOT NULL DEFAULT 0,
    entries_posted  integer      NOT NULL DEFAULT 0,
    entries_skipped integer      NOT NULL DEFAULT 0,
    runs            integer      NOT NULL DEFAULT 1,
    created_by      text,
    version         bigint       NOT NULL DEFAULT 1,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, period)
);

CREATE TABLE people.leave_year_end_run (
    id               uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid          NOT NULL,
    institution_id   uuid          NOT NULL,
    policy_id        uuid          NOT NULL REFERENCES people.leave_policy (id),
    leave_year       integer       NOT NULL,
    status           text          NOT NULL DEFAULT 'completed' CHECK (status IN ('completed')),
    staff_count      integer       NOT NULL DEFAULT 0,
    carried_days     numeric(8,2)  NOT NULL DEFAULT 0,
    expired_days     numeric(8,2)  NOT NULL DEFAULT 0,
    runs             integer       NOT NULL DEFAULT 1,
    created_by       text,
    version          bigint        NOT NULL DEFAULT 1,
    created_at       timestamptz   NOT NULL DEFAULT now(),
    updated_at       timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, leave_year)
);

-- ------------------------------------------------------------------ governed adjustments (E-049)
CREATE TABLE people.leave_adjustment (
    id                   uuid          PRIMARY KEY,
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    staff_id             uuid          NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid          REFERENCES people.staff_engagement (id),
    leave_type_id        uuid          NOT NULL REFERENCES people.leave_type (id),
    leave_type_code      text          NOT NULL,
    leave_year           integer       NOT NULL,
    days                 numeric(6,2)  NOT NULL CHECK (days <> 0),
    reason               text          NOT NULL,
    status               text          NOT NULL CHECK (status IN ('requested', 'pending_approval', 'approved', 'applied', 'rejected')),
    status_reason        text,
    ledger_entry_id      uuid          REFERENCES people.leave_ledger_entry (id),
    process_instance_id  text,
    requested_by         text,
    decided_by           text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX leave_adjustment_staff_idx ON people.leave_adjustment (tenant_id, staff_id);

-- ------------------------------------------------------------------ encashment decisions (E-051)
CREATE TABLE people.leave_encashment (
    id                   uuid          PRIMARY KEY,
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    staff_id             uuid          NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid          REFERENCES people.staff_engagement (id),
    leave_type_id        uuid          NOT NULL REFERENCES people.leave_type (id),
    leave_type_code      text          NOT NULL,
    leave_year           integer       NOT NULL,
    days                 numeric(6,2)  NOT NULL CHECK (days > 0),
    available_days       numeric(6,2)  NOT NULL,
    encashment_cap       numeric(6,2)  NOT NULL,
    reason               text,
    status               text          NOT NULL CHECK (status IN ('requested', 'eligible', 'not_eligible', 'approved', 'paid', 'rejected')),
    status_reason        text,
    -- The `encashment` ledger entry 7C pays (records it in its own encashment_payment table).
    ledger_entry_id      uuid          REFERENCES people.leave_ledger_entry (id),
    process_instance_id  text,
    requested_by         text,
    decided_by           text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX leave_encashment_staff_idx ON people.leave_encashment (tenant_id, staff_id);

-- ------------------------------------------------------------------ leave exceptions (E-052)
CREATE TABLE people.leave_exception (
    id                uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid         NOT NULL,
    institution_id    uuid         NOT NULL,
    number            text         NOT NULL,
    staff_id          uuid         REFERENCES people.staff_member (id),
    leave_request_id  uuid         REFERENCES people.leave_request (id),
    category          text         NOT NULL CHECK (category IN ('balance_shortfall', 'policy_conflict',
                                       'attendance_mismatch', 'backdated_request', 'other')),
    description       text         NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 2000),
    status            text         NOT NULL DEFAULT 'raised' CHECK (status IN ('raised', 'under_review', 'resolved', 'closed')),
    resolution        text,
    reviewed_by       text,
    resolved_by       text,
    resolved_at       timestamptz,
    raised_by         text,
    version           bigint       NOT NULL DEFAULT 1,
    created_at        timestamptz  NOT NULL DEFAULT now(),
    updated_at        timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, number)
);

-- ------------------------------------------------------------------ leave cases (E-034)
CREATE TABLE people.leave_case (
    id                   uuid         PRIMARY KEY,
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id        uuid         NOT NULL REFERENCES people.staff_engagement (id),
    case_type            text         NOT NULL CHECK (case_type IN ('special_leave', 'medical', 'maternity',
                                          'long_leave', 'study_leave', 'other')),
    description          text         NOT NULL,
    -- Leave Management's request the case coordinates (owned by Leave Management, referenced here).
    leave_request_id     uuid         REFERENCES people.leave_request (id),
    -- Outcome reference recorded from Leave Management (`approved`, `rejected`, `cancelled`).
    leave_outcome        text,
    status               text         NOT NULL CHECK (status IN ('initiated', 'pending_approval', 'approved',
                                          'rejected', 'processing', 'completed', 'failed', 'closed', 'archived')),
    status_reason        text,
    process_instance_id  text,
    opened_by            text,
    decided_by           text,
    version              bigint       NOT NULL DEFAULT 1,
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX leave_case_staff_idx ON people.leave_case (tenant_id, staff_id);

-- ------------------------------------------------------------------ long leave (E-006)
CREATE TABLE people.long_leave (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    staff_id        uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id   uuid         NOT NULL REFERENCES people.staff_engagement (id),
    leave_case_id   uuid         REFERENCES people.leave_case (id),
    reason          text         NOT NULL,
    planned_start   date         NOT NULL,
    planned_end     date,
    actual_start    date,
    actual_end      date,
    status          text         NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'active', 'ended', 'cancelled')),
    created_by      text,
    version         bigint       NOT NULL DEFAULT 1,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    updated_at      timestamptz  NOT NULL DEFAULT now(),
    CHECK (planned_end IS NULL OR planned_end >= planned_start)
);
-- At most one planned / running long leave per engagement.
CREATE UNIQUE INDEX long_leave_one_open
    ON people.long_leave (engagement_id) WHERE status IN ('planned', 'active');
