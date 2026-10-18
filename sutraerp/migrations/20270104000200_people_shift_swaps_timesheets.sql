-- ERP_EXTENSIONS 5B — shift swaps, open-shift claims, shift punches, timesheets, payroll inputs
-- and the staff calendar. Logic ported from HeyPuter/everything shifts src/core/swaps.js,
-- attendance.js, timesheet.js, payroll.js, reports.js, ics.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- Builds on 3A (`people.duty_shift`, `people.roster_settings`): a swap / give-away / cover /
-- claim request is agreed by the peer and approved by a manager (never the requester or the
-- peer), and only then reassigns the shift(s) in one transaction under the roster lock. Clock
-- punches are kept per shift (one record per shift, like Everything's attendance) and reconciled
-- with the 7B daily attendance (`people.attendance_day`) into a weekly or monthly timesheet that
-- the staff member submits and a manager approves and locks. Approved totals reach 7C payroll as
-- variable inputs (`people.payroll_variable_input`), read by every payroll calculation.

-- ------------------------------------------------------------------ timesheet + pay settings
-- Indian defaults: Factories Act 1948 s.51/54 (9 hours a day, 48 a week), s.59 (overtime at twice
-- the ordinary rate of wages, ordinary rate = (BASIC + DA) / days / hours per day).
ALTER TABLE people.roster_settings
    ADD COLUMN late_grace_minutes         integer       NOT NULL DEFAULT 10 CHECK (late_grace_minutes BETWEEN 0 AND 240),
    ADD COLUMN early_leave_grace_minutes  integer       NOT NULL DEFAULT 10 CHECK (early_leave_grace_minutes BETWEEN 0 AND 240),
    ADD COLUMN daily_overtime_hours       numeric(4,1)  DEFAULT 9 CHECK (daily_overtime_hours IS NULL OR (daily_overtime_hours > 0 AND daily_overtime_hours <= 24)),
    ADD COLUMN weekly_overtime_hours      numeric(5,1)  DEFAULT 48 CHECK (weekly_overtime_hours IS NULL OR (weekly_overtime_hours > 0 AND weekly_overtime_hours <= 168)),
    ADD COLUMN overtime_multiplier        numeric(4,2)  NOT NULL DEFAULT 2 CHECK (overtime_multiplier >= 1 AND overtime_multiplier <= 5),
    -- A flat ₹ rate per overtime hour; null = derive from BASIC + DA (s.59).
    ADD COLUMN overtime_rate_per_hour     numeric(10,2) CHECK (overtime_rate_per_hour IS NULL OR overtime_rate_per_hour >= 0),
    ADD COLUMN standard_hours_per_day     numeric(4,1)  NOT NULL DEFAULT 8 CHECK (standard_hours_per_day > 0 AND standard_hours_per_day <= 24),
    -- ₹ per night shift (a shift running past midnight, s.57) / per shift worked on a holiday.
    ADD COLUMN night_shift_allowance      numeric(10,2) CHECK (night_shift_allowance IS NULL OR night_shift_allowance >= 0),
    ADD COLUMN holiday_shift_allowance    numeric(10,2) CHECK (holiday_shift_allowance IS NULL OR holiday_shift_allowance >= 0);

-- ------------------------------------------------------------------ swap / cover / claim requests
CREATE TABLE people.duty_shift_swap (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    -- give_away: my shift to a named colleague; swap: my shift for theirs (`other_shift_id`);
    -- cover: my shift to whoever accepts first (or a named colleague); claim: an open shift to me.
    kind                  text          NOT NULL CHECK (kind IN ('give_away', 'swap', 'cover', 'claim')),
    shift_id              uuid          REFERENCES people.duty_shift (id) ON DELETE SET NULL,
    other_shift_id        uuid          REFERENCES people.duty_shift (id) ON DELETE SET NULL,
    from_staff_id         uuid          REFERENCES people.staff_member (id),
    to_staff_id           uuid          REFERENCES people.staff_member (id),
    reason                text          CHECK (reason IS NULL OR length(reason) <= 500),
    -- The shift(s) as they were asked for, so history stays readable after a shift is deleted.
    snapshot              jsonb         NOT NULL,
    status                text          NOT NULL CHECK (status IN ('awaiting_peer', 'awaiting_manager', 'approved', 'declined', 'cancelled')),
    -- Warnings (availability, weekly maximum, working-time limits, usual duty) at the last step.
    warnings              jsonb         NOT NULL DEFAULT '[]'::jsonb,
    requested_by          text          NOT NULL,
    peer_responded_by     text,
    peer_responded_at     timestamptz,
    decided_by            text,
    decided_at            timestamptz,
    decision_reason       text          CHECK (decision_reason IS NULL OR length(decision_reason) <= 500),
    version               bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz   NOT NULL DEFAULT now(),
    updated_at            timestamptz   NOT NULL DEFAULT now(),
    CHECK (from_staff_id IS NULL OR to_staff_id IS NULL OR from_staff_id <> to_staff_id),
    CHECK (kind = 'claim' OR from_staff_id IS NOT NULL)
);
CREATE INDEX duty_shift_swap_institution_idx ON people.duty_shift_swap (tenant_id, institution_id, status);
CREATE INDEX duty_shift_swap_staff_idx ON people.duty_shift_swap (tenant_id, from_staff_id, to_staff_id);
-- One live request per shift.
CREATE UNIQUE INDEX duty_shift_swap_one_live ON people.duty_shift_swap (shift_id)
    WHERE status IN ('awaiting_peer', 'awaiting_manager');

-- ------------------------------------------------------------------ clock punches (one per shift)
CREATE TABLE people.duty_shift_punch (
    shift_id              uuid          PRIMARY KEY REFERENCES people.duty_shift (id) ON DELETE RESTRICT,
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    staff_id              uuid          NOT NULL REFERENCES people.staff_member (id),
    status                text          NOT NULL CHECK (status IN ('present', 'late', 'absent')),
    clock_in              text          CHECK (clock_in IS NULL OR clock_in ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    clock_out             text          CHECK (clock_out IS NULL OR clock_out ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    note                  text          CHECK (note IS NULL OR length(note) <= 500),
    -- self: the staff member's own clock; manager: recorded or corrected by HR.
    source                text          NOT NULL CHECK (source IN ('self', 'manager')),
    recorded_by           text          NOT NULL,
    version               bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz   NOT NULL DEFAULT now(),
    updated_at            timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX duty_shift_punch_staff_idx ON people.duty_shift_punch (tenant_id, staff_id);

-- ------------------------------------------------------------------ timesheets
CREATE TABLE people.timesheet (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    staff_id              uuid          NOT NULL REFERENCES people.staff_member (id),
    period_kind           text          NOT NULL CHECK (period_kind IN ('weekly', 'monthly')),
    period_from           date          NOT NULL,
    period_to             date          NOT NULL,
    -- A submission creates the row; returned = sent back to the staff member to fix.
    status                text          NOT NULL CHECK (status IN ('submitted', 'returned', 'approved', 'locked')),
    scheduled_minutes     numeric(8,1)  NOT NULL DEFAULT 0,
    worked_minutes        numeric(8,1)  NOT NULL DEFAULT 0,
    overtime_minutes      numeric(8,1)  NOT NULL DEFAULT 0,
    late_minutes          integer       NOT NULL DEFAULT 0,
    early_leave_minutes   integer       NOT NULL DEFAULT 0,
    missed_punches        integer       NOT NULL DEFAULT 0,
    absences              integer       NOT NULL DEFAULT 0,
    lop_days              numeric(6,2)  NOT NULL DEFAULT 0,
    night_shifts          integer       NOT NULL DEFAULT 0,
    holiday_shifts        integer       NOT NULL DEFAULT 0,
    -- `{ lines: [reconciled shift lines], totals: {…} }` the columns above came from.
    lines                 jsonb         NOT NULL DEFAULT '{}'::jsonb,
    note                  text          CHECK (note IS NULL OR length(note) <= 500),
    submitted_by          text          NOT NULL,
    submitted_at          timestamptz   NOT NULL DEFAULT now(),
    decided_by            text,
    decided_at            timestamptz,
    decision_reason       text          CHECK (decision_reason IS NULL OR length(decision_reason) <= 500),
    locked_by             text,
    locked_at             timestamptz,
    exported_at           timestamptz,
    version               bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz   NOT NULL DEFAULT now(),
    updated_at            timestamptz   NOT NULL DEFAULT now(),
    CHECK (period_to >= period_from AND period_to - period_from <= 30),
    UNIQUE (staff_id, period_from, period_to)
);
CREATE INDEX timesheet_institution_idx ON people.timesheet (tenant_id, institution_id, period_from);

-- ------------------------------------------------------------------ 7C payroll variable inputs
-- Quantities a payroll calculation reads for the staff member when `work_from` falls inside the
-- salary window: overtime hours, loss-of-pay days not already in daily attendance, night and
-- holiday shifts. Rates come from `people.roster_settings` of the institution.
CREATE TABLE people.payroll_variable_input (
    id                    uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid          NOT NULL,
    institution_id        uuid          NOT NULL,
    staff_id              uuid          NOT NULL REFERENCES people.staff_member (id),
    work_from             date          NOT NULL,
    work_to               date          NOT NULL,
    code                  text          NOT NULL CHECK (code IN ('overtime_hours', 'lop_days', 'night_shifts', 'holiday_shifts')),
    quantity              numeric(8,2)  NOT NULL CHECK (quantity >= 0),
    source                text          NOT NULL DEFAULT 'timesheet' CHECK (source IN ('timesheet')),
    source_id             uuid          NOT NULL,
    status                text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'void')),
    created_by            text          NOT NULL,
    created_at            timestamptz   NOT NULL DEFAULT now(),
    UNIQUE (source_id, code)
);
CREATE INDEX payroll_variable_input_staff_idx ON people.payroll_variable_input (tenant_id, staff_id, work_from);

GRANT SELECT, INSERT, UPDATE, DELETE ON people.duty_shift_swap, people.duty_shift_punch,
    people.timesheet, people.payroll_variable_input TO sutra_people;
SELECT platform.apply_tenant_rls();
