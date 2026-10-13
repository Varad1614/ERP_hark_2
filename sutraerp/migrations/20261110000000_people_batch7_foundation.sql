-- Batch 7 foundation — Domain E (People & workforce) shared tables (docs/batch7-contracts.md §3).
--
-- Foundation-owned. Slices 7A/7B/7C add their own tables in their migration ranges
-- (7A 20261110000010-029, 7B …030-049, 7C …050-069) and never edit this file.
--
-- Money is NUMERIC(14,2), leave days NUMERIC(6,2) (rust_decimal in Rust — never floats); on the
-- wire they are JSON strings. Domain B/D/F ids are plain uuids (no cross-schema FKs).
--
-- Write rules (§3.3):
--   staff_member        7A; rows inserted ONLY via `shared::staff::insert_staff`; status ONLY via
--                       `shared::lifecycle::change_staff_status`.
--   staff_engagement    7A; rows inserted ONLY via `shared::staff::insert_engagement`; status ONLY via
--                       `change_engagement_status` (7A every move, 7B active <-> long_leave only).
--   leave_ledger_entry  7B ONLY via `shared::leave::{post_entry, reverse_entry}` (append-only).
--   attendance_day      7B ONLY via `shared::attendance::record_day` (history in status_history).
--   number_sequence     `shared::numbering::next` only.
--   status_history      `shared::lifecycle` only (append-only).

-- Append-only guard reused by every people history table (slices reuse it too).
CREATE FUNCTION people.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of people.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

-- ------------------------------------------------------------------ staff records (7A)
CREATE TABLE people.staff_member (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    staff_number         text         NOT NULL CHECK (staff_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    -- Login subject (Keycloak `sub`, dev `dev|<loginId>`): self-service + Domain D resolution.
    subject_ref          text         CHECK (subject_ref IS NULL OR length(btrim(subject_ref)) BETWEEN 1 AND 200),
    given_name           text         NOT NULL CHECK (length(btrim(given_name)) BETWEEN 1 AND 100),
    family_name          text         CHECK (family_name IS NULL OR length(btrim(family_name)) BETWEEN 1 AND 100),
    display_name         text         NOT NULL CHECK (length(btrim(display_name)) BETWEEN 1 AND 200),
    work_email           text         CHECK (work_email IS NULL OR work_email ~ '^[^@\s]+@[^@\s]+$'),
    phone                text         CHECK (phone IS NULL OR length(btrim(phone)) BETWEEN 3 AND 32),
    date_of_birth        date,
    gender               text         CHECK (gender IS NULL OR gender IN ('female', 'male', 'other', 'undisclosed')),
    staff_category       text         NOT NULL CHECK (staff_category IN ('teaching', 'non_teaching', 'administrative')),
    -- Duplicate resolution (L3-E-009): the canonical record a merged duplicate points at.
    canonical_staff_id   uuid         REFERENCES people.staff_member (id),
    status               text         NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'pending_verification', 'verified', 'active', 'suspended', 'merged', 'archived')),
    status_reason        text,
    status_changed_at    timestamptz  NOT NULL DEFAULT now(),
    verified_at          timestamptz,
    process_instance_id  text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (canonical_staff_id IS NULL OR canonical_staff_id <> id)
);
CREATE UNIQUE INDEX staff_member_number_per_institution
    ON people.staff_member (tenant_id, institution_id, staff_number);
-- One live staff record per login subject (L3-E-009 duplicates collapse onto it).
CREATE UNIQUE INDEX staff_member_one_live_subject
    ON people.staff_member (tenant_id, subject_ref)
    WHERE subject_ref IS NOT NULL AND status NOT IN ('merged', 'archived');
CREATE INDEX staff_member_institution_idx ON people.staff_member (tenant_id, institution_id, status);
CREATE TRIGGER staff_member_no_delete BEFORE DELETE ON people.staff_member
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ engagements (7A; 7B long leave)
CREATE TABLE people.staff_engagement (
    id                       uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                uuid         NOT NULL,
    institution_id           uuid         NOT NULL,
    staff_id                 uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_number        text         NOT NULL CHECK (engagement_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    -- Domain B org unit (department) / campus, plain ids.
    org_unit_id              uuid,
    campus_id                uuid,
    staff_category           text         NOT NULL CHECK (staff_category IN ('teaching', 'non_teaching', 'administrative')),
    employment_type          text         NOT NULL CHECK (employment_type IN ('permanent', 'probation', 'contract', 'visiting', 'temporary')),
    designation              text         NOT NULL CHECK (length(btrim(designation)) BETWEEN 1 AND 120),
    -- Pay level / grade code (7C maps it to a salary structure).
    grade                    text         CHECK (grade IS NULL OR grade ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$'),
    start_date               date         NOT NULL,
    end_date                 date,
    last_working_date        date,
    probation_end_date       date,
    -- Reporting manager (L3-E-018; 7B manager approvals).
    reports_to_staff_id      uuid         REFERENCES people.staff_member (id),
    rejoin_of_engagement_id  uuid         REFERENCES people.staff_engagement (id),
    status                   text         NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'active', 'suspended', 'deputed', 'long_leave', 'closed', 'cancelled')),
    status_reason            text,
    status_changed_at        timestamptz  NOT NULL DEFAULT now(),
    process_instance_id      text,
    version                  bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at               timestamptz  NOT NULL DEFAULT now(),
    updated_at               timestamptz  NOT NULL DEFAULT now(),
    CHECK (end_date IS NULL OR end_date >= start_date),
    CHECK (last_working_date IS NULL OR last_working_date >= start_date),
    CHECK (reports_to_staff_id IS NULL OR reports_to_staff_id <> staff_id)
);
CREATE UNIQUE INDEX staff_engagement_number_per_institution
    ON people.staff_engagement (tenant_id, institution_id, engagement_number);
-- One live engagement per staff member (rejoining opens a new one after closure, L3-E-008).
CREATE UNIQUE INDEX staff_engagement_one_live
    ON people.staff_engagement (staff_id) WHERE status NOT IN ('closed', 'cancelled');
CREATE INDEX staff_engagement_institution_idx ON people.staff_engagement (tenant_id, institution_id, status);
CREATE INDEX staff_engagement_manager_idx ON people.staff_engagement (tenant_id, reports_to_staff_id);
CREATE TRIGGER staff_engagement_no_delete BEFORE DELETE ON people.staff_engagement
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ leave ledger (7B, append-only)
CREATE TABLE people.leave_ledger_entry (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    staff_id           uuid          NOT NULL REFERENCES people.staff_member (id),
    engagement_id      uuid          REFERENCES people.staff_engagement (id),
    -- 7B leave type (slice table, plain id) + code snapshot.
    leave_type_id      uuid          NOT NULL,
    leave_type_code    text          NOT NULL CHECK (leave_type_code ~ '^[A-Z0-9][A-Z0-9_-]{0,15}$'),
    is_paid            boolean       NOT NULL DEFAULT true,
    kind               text          NOT NULL CHECK (kind IN ('opening', 'accrual', 'carry_forward', 'adjustment',
                                         'reservation', 'release', 'consumption', 'expiry', 'encashment', 'reversal')),
    days               numeric(6,2)  NOT NULL CHECK (days <> 0),
    effective_on       date          NOT NULL,
    leave_year         integer       NOT NULL CHECK (leave_year BETWEEN 2000 AND 2200),
    source_type        text          NOT NULL CHECK (source_type ~ '^[a-z][a-z0-9_]{1,47}$'),
    source_id          uuid          NOT NULL,
    posting_key        text          NOT NULL CHECK (length(btrim(posting_key)) BETWEEN 1 AND 100),
    reverses_entry_id  uuid          REFERENCES people.leave_ledger_entry (id),
    reason             text,
    actor              text,
    correlation_id     text,
    created_at         timestamptz   NOT NULL DEFAULT now(),
    CHECK (kind IN ('opening', 'accrual', 'carry_forward', 'release') AND days > 0
        OR kind IN ('reservation', 'consumption', 'expiry', 'encashment') AND days < 0
        OR kind IN ('adjustment', 'reversal')),
    CHECK ((kind = 'reversal') = (reverses_entry_id IS NOT NULL))
);
-- Idempotency key: a retried command never posts twice.
CREATE UNIQUE INDEX leave_ledger_entry_idempotency
    ON people.leave_ledger_entry (tenant_id, source_type, source_id, posting_key);
-- An entry is reversed at most once.
CREATE UNIQUE INDEX leave_ledger_entry_one_reversal
    ON people.leave_ledger_entry (reverses_entry_id) WHERE reverses_entry_id IS NOT NULL;
CREATE INDEX leave_ledger_entry_balance_idx
    ON people.leave_ledger_entry (tenant_id, staff_id, leave_year, leave_type_id);
CREATE INDEX leave_ledger_entry_period_idx
    ON people.leave_ledger_entry (tenant_id, staff_id, effective_on);
CREATE TRIGGER leave_ledger_entry_append_only BEFORE UPDATE OR DELETE ON people.leave_ledger_entry
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ attendance days (7B)
CREATE TABLE people.attendance_day (
    id                uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid         NOT NULL,
    institution_id    uuid         NOT NULL,
    staff_id          uuid         NOT NULL REFERENCES people.staff_member (id),
    work_date         date         NOT NULL,
    status            text         NOT NULL CHECK (status IN ('present', 'absent', 'half_day', 'on_leave', 'holiday', 'weekly_off')),
    source            text         NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'bulk', 'leave', 'correction')),
    -- 7B leave request that marked the day `on_leave` (plain id).
    leave_request_id  uuid,
    remark            text,
    version           bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at        timestamptz  NOT NULL DEFAULT now(),
    updated_at        timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (staff_id, work_date)
);
CREATE INDEX attendance_day_period_idx ON people.attendance_day (tenant_id, staff_id, work_date);
CREATE TRIGGER attendance_day_no_delete BEFORE DELETE ON people.attendance_day
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ numbering + status history
CREATE TABLE people.number_sequence (
    tenant_id        uuid    NOT NULL,
    institution_id   uuid    NOT NULL,
    kind             text    NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{1,47}$'),
    next_value       bigint  NOT NULL CHECK (next_value >= 1),
    PRIMARY KEY (tenant_id, institution_id, kind)
);

CREATE TABLE people.status_history (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    aggregate_type       text         NOT NULL,
    aggregate_id         uuid         NOT NULL,
    from_status          text,
    to_status            text         NOT NULL,
    reason               text,
    cause_event_type     text,
    process_instance_id  text,
    actor                text,
    correlation_id       text,
    created_at           timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX people_status_history_aggregate_idx
    ON people.status_history (tenant_id, aggregate_type, aggregate_id, created_at);
CREATE TRIGGER people_status_history_append_only BEFORE UPDATE OR DELETE ON people.status_history
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();

-- ------------------------------------------------------------------ runtime role (sutra_people)
-- The role and the schema default privileges come from 20261014000003_runtime_db_roles.sql; the
-- grants are restated here so the foundation tables are covered on clusters provisioned out of
-- band, and the ledger / history tables are append-only at the privilege level as well.
GRANT USAGE ON SCHEMA people TO sutra_people;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA people TO sutra_people;
REVOKE UPDATE, DELETE, TRUNCATE ON people.leave_ledger_entry, people.status_history FROM sutra_people;
GRANT INSERT ON platform.outbox, platform.audit_log TO sutra_people;
