-- Batch 2 slice 2C: academic calendar (L2-B05; L3/L4-B-027 … B-033).
--
-- Business-local dates are `date` and are interpreted in the institution business timezone
-- (`institution.institution.timezone`, ADR-L5-0036 §3.4); a published calendar version snapshots
-- that zone in `calendar_version.timezone`. Instants are `timestamptz`.
--
-- Nothing here is ever deleted by the application: retirement, supersession, closure and
-- completion are state changes, so historical calendar interpretation stays reproducible
-- (L4-B-033-BH-006). `calendar_assignment.scope_id` is a plain uuid (no FK across slices).

CREATE TABLE institution.academic_calendar (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL REFERENCES institution.institution (id),
    code            text        NOT NULL,
    name            text        NOT NULL,
    description     text,
    status          text        NOT NULL DEFAULT 'draft'
        CONSTRAINT academic_calendar_status_check CHECK (status IN ('draft', 'in_review', 'published', 'superseded', 'retired')),
    status_reason   text,
    retired_at      timestamptz,
    version         bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT academic_calendar_code_unique UNIQUE (tenant_id, institution_id, code)
);

CREATE TABLE institution.academic_year (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_id          uuid        NOT NULL REFERENCES institution.academic_calendar (id),
    label                text        NOT NULL,
    starts_on            date        NOT NULL,
    ends_on              date        NOT NULL,
    status               text        NOT NULL DEFAULT 'planned'
        CONSTRAINT academic_year_status_check CHECK (status IN ('planned', 'open', 'closing', 'closed')),
    predecessor_year_id  uuid        REFERENCES institution.academic_year (id),
    opened_at            timestamptz,
    closed_at            timestamptz,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT academic_year_range_check CHECK (ends_on > starts_on),
    CONSTRAINT academic_year_label_unique UNIQUE (tenant_id, calendar_id, label)
);
CREATE INDEX academic_year_institution_idx ON institution.academic_year (tenant_id, institution_id);
-- At most one open academic context per calendar.
CREATE UNIQUE INDEX academic_year_one_open_per_calendar
    ON institution.academic_year (calendar_id) WHERE status IN ('open', 'closing');

CREATE TABLE institution.calendar_version (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_id          uuid        NOT NULL REFERENCES institution.academic_calendar (id),
    academic_year_id     uuid        NOT NULL REFERENCES institution.academic_year (id),
    version_no           integer     NOT NULL CHECK (version_no >= 1),
    status               text        NOT NULL DEFAULT 'draft'
        CONSTRAINT calendar_version_status_check CHECK (status IN ('draft', 'in_review', 'published', 'superseded', 'retired')),
    supersedes_id        uuid        REFERENCES institution.calendar_version (id),
    starts_on            date        NOT NULL,
    ends_on              date        NOT NULL,
    -- IANA zone snapshot taken at publication (NULL while draft: the live institution zone applies).
    timezone             text,
    notes                text,
    process_instance_id  text,
    status_reason        text,
    published_at         timestamptz,
    retired_at           timestamptz,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT calendar_version_range_check CHECK (ends_on > starts_on),
    CONSTRAINT calendar_version_no_unique UNIQUE (calendar_id, version_no)
);
CREATE INDEX calendar_version_institution_idx ON institution.calendar_version (tenant_id, institution_id);
-- One editable draft and one governing published version per calendar + academic year.
CREATE UNIQUE INDEX calendar_version_one_draft
    ON institution.calendar_version (calendar_id, academic_year_id) WHERE status IN ('draft', 'in_review');
CREATE UNIQUE INDEX calendar_version_one_published
    ON institution.calendar_version (calendar_id, academic_year_id) WHERE status = 'published';

CREATE TABLE institution.academic_term (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_version_id  uuid        NOT NULL REFERENCES institution.calendar_version (id),
    code                 text        NOT NULL,
    name                 text        NOT NULL,
    sequence             integer     NOT NULL CHECK (sequence >= 1),
    starts_on            date        NOT NULL,
    ends_on              date        NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT academic_term_range_check CHECK (ends_on > starts_on),
    CONSTRAINT academic_term_code_unique UNIQUE (calendar_version_id, code)
);

CREATE TABLE institution.calendar_key_date (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_version_id  uuid        NOT NULL REFERENCES institution.calendar_version (id),
    kind                 text        NOT NULL
        CONSTRAINT calendar_key_date_kind_check CHECK (kind IN (
            'holiday', 'instruction_start', 'instruction_end', 'exam_window',
            'registration_window', 'break', 'event')),
    name                 text        NOT NULL,
    starts_on            date        NOT NULL,
    ends_on              date        NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT calendar_key_date_range_check CHECK (ends_on >= starts_on)
);
CREATE INDEX calendar_key_date_version_idx ON institution.calendar_key_date (calendar_version_id);

CREATE TABLE institution.calendar_assignment (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_id          uuid        NOT NULL REFERENCES institution.academic_calendar (id),
    calendar_version_id  uuid        NOT NULL REFERENCES institution.calendar_version (id),
    scope_kind           text        NOT NULL
        CONSTRAINT calendar_assignment_scope_kind_check CHECK (scope_kind IN ('institution', 'campus', 'org_unit', 'programme')),
    scope_id             uuid        NOT NULL,
    effective_from       date        NOT NULL,
    effective_to         date,
    status               text        NOT NULL DEFAULT 'draft'
        CONSTRAINT calendar_assignment_status_check CHECK (status IN ('draft', 'effective', 'retired')),
    status_reason        text,
    activated_at         timestamptz,
    retired_at           timestamptz,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT calendar_assignment_range_check CHECK (effective_to IS NULL OR effective_to >= effective_from)
);
CREATE INDEX calendar_assignment_scope_idx
    ON institution.calendar_assignment (tenant_id, institution_id, scope_kind, scope_id);
CREATE INDEX calendar_assignment_version_idx ON institution.calendar_assignment (calendar_version_id);

CREATE TABLE institution.calendar_exception (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_id          uuid        NOT NULL REFERENCES institution.academic_calendar (id),
    calendar_version_id  uuid        NOT NULL REFERENCES institution.calendar_version (id),
    kind                 text        NOT NULL
        CONSTRAINT calendar_exception_kind_check CHECK (kind IN (
            'closure', 'rescheduled_working_day', 'extended_term', 'additional_holiday', 'other')),
    scope_kind           text        NOT NULL DEFAULT 'institution'
        CONSTRAINT calendar_exception_scope_kind_check CHECK (scope_kind IN ('institution', 'campus', 'org_unit', 'programme')),
    scope_id             uuid        NOT NULL,
    starts_on            date        NOT NULL,
    ends_on              date        NOT NULL,
    reason               text        NOT NULL,
    owner_subject        text        NOT NULL,
    status               text        NOT NULL DEFAULT 'requested'
        CONSTRAINT calendar_exception_status_check CHECK (status IN (
            'requested', 'approved', 'rejected', 'in_effect', 'completed', 'withdrawn')),
    process_instance_id  text,
    decision_reason      text,
    decided_at           timestamptz,
    applied_at           timestamptz,
    completed_at         timestamptz,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT calendar_exception_range_check CHECK (ends_on >= starts_on)
);
CREATE INDEX calendar_exception_calendar_idx ON institution.calendar_exception (tenant_id, calendar_id);

CREATE TABLE institution.calendar_simulation (
    id                   uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid        NOT NULL,
    institution_id       uuid        NOT NULL REFERENCES institution.institution (id),
    calendar_id          uuid        NOT NULL REFERENCES institution.academic_calendar (id),
    baseline_version_id  uuid        NOT NULL REFERENCES institution.calendar_version (id),
    status               text        NOT NULL DEFAULT 'draft'
        CONSTRAINT calendar_simulation_status_check CHECK (status IN ('draft', 'evaluated', 'accepted', 'discarded')),
    -- Immutable point-in-time inputs (L4-B-032-BH-001/-007) and the impact report.
    baseline             jsonb       NOT NULL,
    proposal             jsonb       NOT NULL,
    impact               jsonb       NOT NULL,
    outcome              text        NOT NULL
        CONSTRAINT calendar_simulation_outcome_check CHECK (outcome IN ('favourable', 'unfavourable', 'uncertain')),
    created_by           text,
    decision_reason      text,
    decided_by           text,
    decided_at           timestamptz,
    version              bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX calendar_simulation_calendar_idx ON institution.calendar_simulation (tenant_id, calendar_id);

CREATE TABLE institution.academic_year_closure (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    institution_id        uuid        NOT NULL REFERENCES institution.institution (id),
    academic_year_id      uuid        NOT NULL REFERENCES institution.academic_year (id),
    successor_year_id     uuid        REFERENCES institution.academic_year (id),
    successor_version_id  uuid        REFERENCES institution.calendar_version (id),
    evidence              jsonb       NOT NULL,
    early_closure_reason  text,
    process_instance_id   text,
    closed_by             text,
    closed_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT academic_year_closure_once UNIQUE (academic_year_id)
);
