-- ERP_EXTENSIONS 3A — duty rosters. Logic ported from HeyPuter/everything shifts
-- src/core/scheduling.js, rules.js, fill.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- A duty shift is a dated stretch of work in a role (warden, security, invigilation, lab…),
-- open until a staff member holds it; an end before the start runs past midnight. Overlap on
-- one person is the only hard rule. Weekly availability, hour targets / limits and the
-- institution's working-time limits are warnings; approved leave counts as time off.
CREATE TABLE people.roster_settings (
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    min_rest_hours        numeric(4,1)  CHECK (min_rest_hours IS NULL OR (min_rest_hours > 0 AND min_rest_hours <= 24)),
    max_daily_hours       numeric(4,1)  CHECK (max_daily_hours IS NULL OR (max_daily_hours > 0 AND max_daily_hours <= 24)),
    max_consecutive_days  integer       CHECK (max_consecutive_days IS NULL OR max_consecutive_days BETWEEN 1 AND 60),
    version               bigint        NOT NULL DEFAULT 1,
    updated_at            timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

CREATE TABLE people.roster_requirement (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    role                  text          NOT NULL CHECK (length(btrim(role)) BETWEEN 1 AND 80),
    days                  integer[]     NOT NULL CHECK (days <@ ARRAY[0,1,2,3,4,5,6] AND cardinality(days) >= 1),
    head_count            integer       NOT NULL CHECK (head_count BETWEEN 1 AND 100),
    start_time            text          CHECK (start_time IS NULL OR start_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    end_time              text          CHECK (end_time IS NULL OR end_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    created_at            timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX roster_requirement_institution_idx ON people.roster_requirement (tenant_id, institution_id);

CREATE TABLE people.roster_staff_profile (
    staff_id              uuid          PRIMARY KEY REFERENCES people.staff_member (id),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    role                  text          CHECK (role IS NULL OR length(btrim(role)) BETWEEN 1 AND 80),
    target_hours          numeric(5,1)  CHECK (target_hours IS NULL OR (target_hours >= 0 AND target_hours <= 168)),
    max_hours             numeric(5,1)  CHECK (max_hours IS NULL OR (max_hours > 0 AND max_hours <= 168)),
    -- Seven { off, start, end } windows, Sunday first; null = available any time.
    days                  jsonb         CHECK (days IS NULL OR (jsonb_typeof(days) = 'array' AND jsonb_array_length(days) = 7)),
    rosterable            boolean       NOT NULL DEFAULT true,
    version               bigint        NOT NULL DEFAULT 1,
    updated_at            timestamptz   NOT NULL DEFAULT now()
);

CREATE TABLE people.duty_shift (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    shift_date            date          NOT NULL,
    start_time            text          NOT NULL CHECK (start_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    end_time              text          NOT NULL CHECK (end_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$' AND end_time <> start_time),
    break_minutes         integer       NOT NULL DEFAULT 0 CHECK (break_minutes >= 0 AND break_minutes < 1440),
    role                  text          NOT NULL CHECK (length(btrim(role)) BETWEEN 1 AND 80),
    location              text          CHECK (location IS NULL OR length(location) <= 200),
    note                  text          CHECK (note IS NULL OR length(note) <= 500),
    staff_id              uuid          REFERENCES people.staff_member (id),
    status                text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published')),
    version               bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz   NOT NULL DEFAULT now(),
    updated_at            timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX duty_shift_institution_idx ON people.duty_shift (tenant_id, institution_id, shift_date);
CREATE INDEX duty_shift_staff_idx ON people.duty_shift (tenant_id, staff_id, shift_date) WHERE staff_id IS NOT NULL;

GRANT SELECT, INSERT, UPDATE, DELETE ON people.roster_settings, people.roster_requirement,
    people.roster_staff_profile, people.duty_shift TO sutra_people;
SELECT platform.apply_tenant_rls();
