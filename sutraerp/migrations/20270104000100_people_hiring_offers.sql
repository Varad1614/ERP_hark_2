-- ERP_EXTENSIONS 5A — offer letters, onboarding checklists, follow-ups / reminders, hiring
-- analytics settings and the public careers page. Logic ported from HeyPuter/everything hiring
-- src/core/offers.js, onboard.js, followups.js, analytics.js, pipeline.js, retention.js,
-- reminders.js and jobpage.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- An offer is drafted on a candidate with a monthly CTC breakup in ₹, goes to approval (the
-- approver must not be the drafter / submitter — DB CHECK below plus the handler guard), is sent
-- (the letter text is snapshotted), and ends accepted / declined / expired / withdrawn. An
-- accepted offer moves the candidate to offer_accepted, and the existing onboarding case stamps
-- its checklist from the institution's templates (due offsets from the joining date).

-- ------------------------------------------------------------------ settings
ALTER TABLE people.hiring_settings
    ADD COLUMN offer_template        text     CHECK (offer_template IS NULL OR length(offer_template) <= 20000),
    ADD COLUMN onboarding_templates  jsonb    NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(onboarding_templates) = 'array'),
    ADD COLUMN stale_after_days      integer  NOT NULL DEFAULT 14 CHECK (stale_after_days BETWEEN 1 AND 365),
    ADD COLUMN retention_months      integer  NOT NULL DEFAULT 0 CHECK (retention_months BETWEEN 0 AND 120),
    ADD COLUMN careers_enabled       boolean  NOT NULL DEFAULT false,
    ADD COLUMN careers_slug          text     CHECK (careers_slug IS NULL OR careers_slug ~ '^[a-z0-9][a-z0-9-]{1,38}[a-z0-9]$'),
    ADD COLUMN careers_intro         text     CHECK (careers_intro IS NULL OR length(careers_intro) <= 2000),
    ADD COLUMN careers_contact_email text     CHECK (careers_contact_email IS NULL OR careers_contact_email ~ '^[^@\s]+@[^@\s]+$');
-- A careers slug addresses one institution across every tenant (public URL).
CREATE UNIQUE INDEX hiring_settings_careers_slug ON people.hiring_settings (careers_slug) WHERE careers_slug IS NOT NULL;

-- ------------------------------------------------------------------ requisition / candidate
ALTER TABLE people.requisition
    ADD COLUMN careers_published   boolean  NOT NULL DEFAULT false,
    ADD COLUMN public_description  text     CHECK (public_description IS NULL OR length(public_description) <= 6000),
    ADD COLUMN public_location     text     CHECK (public_location IS NULL OR length(public_location) <= 120),
    ADD COLUMN closes_on           date;
ALTER TABLE people.candidate
    ADD COLUMN talent_pool         boolean  NOT NULL DEFAULT false;

-- ------------------------------------------------------------------ offers
CREATE TABLE people.candidate_offer (
    id                uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid           NOT NULL,
    institution_id    uuid           NOT NULL,
    candidate_id      uuid           NOT NULL REFERENCES people.candidate (id),
    requisition_id    uuid           NOT NULL REFERENCES people.requisition (id),
    offer_number      text           NOT NULL CHECK (offer_number ~ '^[A-Z]{2,5}-[0-9]{6,}$'),
    designation       text           NOT NULL CHECK (length(btrim(designation)) BETWEEN 1 AND 120),
    grade             text           CHECK (grade IS NULL OR length(grade) <= 60),
    department        text           CHECK (department IS NULL OR length(department) <= 120),
    joining_date      date           NOT NULL,
    expires_on        date           CHECK (expires_on IS NULL OR expires_on <= joining_date),
    probation_months  integer        CHECK (probation_months IS NULL OR probation_months BETWEEN 0 AND 36),
    basic             numeric(12, 2) NOT NULL CHECK (basic > 0),
    hra               numeric(12, 2) NOT NULL DEFAULT 0 CHECK (hra >= 0),
    da                numeric(12, 2) NOT NULL DEFAULT 0 CHECK (da >= 0),
    allowances        numeric(12, 2) NOT NULL DEFAULT 0 CHECK (allowances >= 0),
    employer_pf       numeric(12, 2) NOT NULL DEFAULT 0 CHECK (employer_pf >= 0),
    joining_bonus     numeric(12, 2) CHECK (joining_bonus IS NULL OR joining_bonus >= 0),
    notes             text           CHECK (notes IS NULL OR length(notes) <= 2000),
    status            text           NOT NULL DEFAULT 'draft'
        CHECK (status IN ('draft', 'pending_approval', 'approved', 'sent', 'accepted', 'declined', 'expired', 'withdrawn')),
    status_reason     text,
    letter_text       text,
    drafted_by        text,
    submitted_by      text,
    decided_by        text,
    decided_at        timestamptz,
    sent_at           timestamptz,
    accepted_by_name  text           CHECK (accepted_by_name IS NULL OR length(btrim(accepted_by_name)) BETWEEN 1 AND 120),
    resolved_at       timestamptz,
    version           bigint         NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at        timestamptz    NOT NULL DEFAULT now(),
    updated_at        timestamptz    NOT NULL DEFAULT now(),
    -- Separation of duties: whoever drafted or submitted the offer cannot approve it.
    CHECK (decided_by IS NULL OR (decided_by IS DISTINCT FROM submitted_by AND decided_by IS DISTINCT FROM drafted_by)),
    CHECK (status NOT IN ('sent', 'accepted', 'declined', 'expired') OR sent_at IS NOT NULL)
);
-- At most one offer in play per candidate (Everything's isOpenOffer).
CREATE UNIQUE INDEX candidate_offer_one_open ON people.candidate_offer (candidate_id)
    WHERE status IN ('draft', 'pending_approval', 'approved', 'sent');
CREATE INDEX candidate_offer_institution_idx ON people.candidate_offer (tenant_id, institution_id, status);
CREATE INDEX candidate_offer_expiry_idx ON people.candidate_offer (status, expires_on) WHERE status = 'sent';

-- ------------------------------------------------------------------ onboarding checklist
CREATE TABLE people.onboarding_task (
    id                  uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid         NOT NULL,
    institution_id      uuid         NOT NULL,
    onboarding_case_id  uuid         NOT NULL REFERENCES people.onboarding_case (id),
    position            integer      NOT NULL CHECK (position >= 0),
    title               text         NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 140),
    category            text         NOT NULL CHECK (length(btrim(category)) BETWEEN 1 AND 40),
    owner               text         CHECK (owner IS NULL OR owner IN ('hr', 'it', 'department', 'accounts', 'admin')),
    offset_days         integer      CHECK (offset_days IS NULL OR offset_days BETWEEN -365 AND 365),
    template_id         text,
    done                boolean      NOT NULL DEFAULT false,
    done_at             timestamptz,
    done_by             text,
    created_at          timestamptz  NOT NULL DEFAULT now(),
    CHECK (done = (done_at IS NOT NULL))
);
CREATE INDEX onboarding_task_case_idx ON people.onboarding_task (tenant_id, onboarding_case_id, position);

-- ------------------------------------------------------------------ follow-ups and reminders
CREATE TABLE people.candidate_followup (
    id              uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    candidate_id    uuid         NOT NULL REFERENCES people.candidate (id),
    text            text         NOT NULL CHECK (length(btrim(text)) BETWEEN 1 AND 500),
    due_date        date         NOT NULL,
    due_time        text         CHECK (due_time IS NULL OR due_time ~ '^([01]?[0-9]|2[0-3]):[0-5][0-9]$'),
    done            boolean      NOT NULL DEFAULT false,
    completed_at    timestamptz,
    created_by      text,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    CHECK (done = (completed_at IS NOT NULL))
);
CREATE INDEX candidate_followup_candidate_idx ON people.candidate_followup (tenant_id, candidate_id);
CREATE INDEX candidate_followup_open_idx ON people.candidate_followup (tenant_id, institution_id, due_date) WHERE NOT done;

-- Reminders recorded by the hourly job; the stable key (Everything's dedupe key) records each
-- nudge once.
CREATE TABLE people.hiring_reminder (
    tenant_id       uuid         NOT NULL,
    institution_id  uuid         NOT NULL,
    reminder_key    text         NOT NULL CHECK (length(reminder_key) BETWEEN 1 AND 200),
    kind            text         NOT NULL CHECK (kind IN ('offer', 'followup', 'onboarding')),
    candidate_id    uuid         REFERENCES people.candidate (id),
    title           text         NOT NULL,
    body            text         NOT NULL,
    created_at      timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, reminder_key)
);
CREATE INDEX hiring_reminder_recent_idx ON people.hiring_reminder (tenant_id, institution_id, created_at DESC);

GRANT SELECT, INSERT, UPDATE, DELETE ON people.candidate_offer, people.onboarding_task,
    people.candidate_followup, people.hiring_reminder TO sutra_people;
SELECT platform.apply_tenant_rls();
