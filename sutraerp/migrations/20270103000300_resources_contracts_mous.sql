-- ERP_EXTENSIONS 3C — contracts and MoUs. Logic ported from HeyPuter/everything contracts
-- src/lib/dates.js, store.js, ical.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- A contract (vendor agreement, AMC, lease, licence, NDA) or MoU (academic, industry, research)
-- has a term clock: the end of its first term is the anchor every auto-renewal is counted from.
-- A recorded decision (notice given / renewing / not renewing / snoozed) answers one term end.
-- Finished terms are recorded once each. Obligations say who owes what and when, and may repeat
-- on a month cadence (one live occurrence; completed ones kept as history). MoU activities are
-- the evidence NAAC asks for (functional MoUs with activities, criterion 3.7).
CREATE TABLE resources.contract (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    title              text          NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    kind               text          NOT NULL CHECK (kind IN ('vendor', 'service', 'amc', 'lease',
                           'license', 'nda', 'mou_academic', 'mou_industry', 'mou_research',
                           'other')),
    status             text          NOT NULL DEFAULT 'active'
                           CHECK (status IN ('active', 'draft', 'terminated')),
    supplier_id        uuid          REFERENCES resources.supplier (id),
    counterparty_name  text          NOT NULL CHECK (length(btrim(counterparty_name)) BETWEEN 1 AND 200),
    counterparty_scope text          NOT NULL DEFAULT 'national'
                           CHECK (counterparty_scope IN ('national', 'international')),
    contact_name       text          CHECK (contact_name IS NULL OR length(contact_name) <= 120),
    contact_email      text          CHECK (contact_email IS NULL OR length(contact_email) <= 200),
    contact_phone      text          CHECK (contact_phone IS NULL OR length(contact_phone) <= 40),
    value              numeric(16,2) CHECK (value IS NULL OR value >= 0),
    currency           text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    billing            text          NOT NULL DEFAULT 'one-time'
                           CHECK (billing IN ('one-time', 'monthly', 'quarterly', 'annual')),
    start_on           date,
    end_on             date,
    auto_renew         boolean       NOT NULL DEFAULT false,
    term_months        integer       NOT NULL DEFAULT 12 CHECK (term_months BETWEEN 1 AND 120),
    notice_days        integer       NOT NULL DEFAULT 0 CHECK (notice_days BETWEEN 0 AND 730),
    purposes           text[]        NOT NULL DEFAULT '{}',
    tags               text[]        NOT NULL DEFAULT '{}',
    notes              text          CHECK (notes IS NULL OR length(notes) <= 4000),
    -- { kind, date, note, forTermEnd, until } or NULL.
    decision           jsonb         CHECK (decision IS NULL OR jsonb_typeof(decision) = 'object'),
    version            bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by         text,
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT contract_term_check CHECK (start_on IS NULL OR end_on IS NULL OR end_on >= start_on)
);
CREATE INDEX contract_institution_idx ON resources.contract (tenant_id, institution_id, status);
CREATE INDEX contract_supplier_idx ON resources.contract (tenant_id, supplier_id);

-- Finished terms, recorded once each (append-only).
CREATE TABLE resources.contract_term (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    contract_id        uuid          NOT NULL REFERENCES resources.contract (id),
    start_on           date,
    end_on             date          NOT NULL,
    ended              text          NOT NULL CHECK (ended IN ('renewed', 'notice-given', 'expired', 'terminated')),
    value              numeric(16,2),
    currency           text          NOT NULL DEFAULT 'INR',
    note               text          NOT NULL DEFAULT '',
    recorded_at        timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT contract_term_once UNIQUE (tenant_id, contract_id, end_on)
);
REVOKE UPDATE, DELETE, TRUNCATE ON resources.contract_term FROM sutra_resources;

CREATE TABLE resources.contract_obligation (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    contract_id        uuid          NOT NULL REFERENCES resources.contract (id),
    text               text          NOT NULL CHECK (length(btrim(text)) BETWEEN 1 AND 200),
    owner              text          NOT NULL DEFAULT 'us' CHECK (owner IN ('us', 'them', 'mutual')),
    due_on             date,
    done               boolean       NOT NULL DEFAULT false,
    -- { every, anchor, until, count, contractEnd } or NULL.
    repeat             jsonb         CHECK (repeat IS NULL OR jsonb_typeof(repeat) = 'object'),
    -- Completed occurrences, oldest first: [{ due, doneAt, by }] (at most 120).
    history            jsonb         NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(history) = 'array'),
    version            bigint        NOT NULL DEFAULT 1,
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX contract_obligation_contract_idx ON resources.contract_obligation (tenant_id, contract_id);

-- What happened under an MoU (or any contract): the NAAC evidence of a functional MoU.
CREATE TABLE resources.contract_activity (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    contract_id        uuid          NOT NULL REFERENCES resources.contract (id),
    held_on            date          NOT NULL,
    kind               text          NOT NULL CHECK (kind IN ('internship', 'on_job_training',
                           'project_work', 'student_exchange', 'faculty_exchange',
                           'collaborative_research', 'workshop', 'training', 'consultancy',
                           'placement', 'publication', 'other')),
    title              text          NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    students           integer       NOT NULL DEFAULT 0 CHECK (students >= 0),
    staff              integer       NOT NULL DEFAULT 0 CHECK (staff >= 0),
    outcome            text          CHECK (outcome IS NULL OR length(outcome) <= 2000),
    evidence_ref       text          CHECK (evidence_ref IS NULL OR length(evidence_ref) <= 500),
    created_by         text,
    created_at         timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX contract_activity_idx ON resources.contract_activity (tenant_id, institution_id, held_on);

CREATE TABLE resources.contract_settings (
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    soon_days          integer       NOT NULL DEFAULT 60 CHECK (soon_days BETWEEN 1 AND 365),
    obligation_days    integer       NOT NULL DEFAULT 30 CHECK (obligation_days BETWEEN 1 AND 365),
    default_notice_days integer      NOT NULL DEFAULT 30 CHECK (default_notice_days BETWEEN 0 AND 730),
    alarm_days         integer[]     NOT NULL DEFAULT '{7,1}',
    version            bigint        NOT NULL DEFAULT 1,
    updated_by         text,
    updated_at         timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON resources.contract, resources.contract_obligation,
    resources.contract_activity, resources.contract_settings TO sutra_resources;
GRANT SELECT, INSERT ON resources.contract_term TO sutra_resources;
SELECT platform.apply_tenant_rls();
