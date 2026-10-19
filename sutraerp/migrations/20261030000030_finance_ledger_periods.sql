-- Batch 5 slice 5C — financial years and periods (L2-F09, L3-F-051…055), manual journals
-- (L2-F05, L3-F-025…026/028), source ↔ ledger reconciliation (F-029) and trial balance snapshots
-- (F-030/031). The ledger itself (`finance.journal_entry` / `journal_line`) stays foundation-owned
-- and is written ONLY through `shared::posting`; these tables carry the governance around it.

-- ------------------------------------------------------------------ financial years / periods
CREATE TABLE finance.financial_year (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    code                 text          NOT NULL CHECK (code ~ '^[0-9A-Z][0-9A-Z._-]{1,31}$'),
    name                 text          NOT NULL CHECK (length(btrim(name)) > 0),
    start_date           date          NOT NULL,
    end_date             date          NOT NULL,
    status               text          NOT NULL DEFAULT 'draft'
                                       CHECK (status IN ('draft', 'open', 'closing', 'closed', 'archived')),
    status_reason        text,
    process_instance_id  text,
    closed_at            timestamptz,
    closed_by            text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (end_date > start_date),
    CHECK (end_date <= start_date + 400)
);
CREATE UNIQUE INDEX financial_year_code_per_institution
    ON finance.financial_year (tenant_id, institution_id, code);
CREATE INDEX financial_year_dates_idx
    ON finance.financial_year (tenant_id, institution_id, start_date, end_date);

CREATE TABLE finance.financial_period (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    financial_year_id    uuid          NOT NULL REFERENCES finance.financial_year (id),
    period_no            integer       NOT NULL CHECK (period_no BETWEEN 1 AND 13),
    name                 text          NOT NULL,
    start_date           date          NOT NULL,
    end_date             date          NOT NULL,
    status               text          NOT NULL DEFAULT 'planned'
                                       CHECK (status IN ('planned', 'open', 'locked', 'closed', 'reopened', 'archived')),
    status_reason        text,
    process_instance_id  text,
    closed_at            timestamptz,
    closed_by            text,
    reopen_count         integer       NOT NULL DEFAULT 0,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (end_date >= start_date),
    UNIQUE (financial_year_id, period_no)
);
-- The period guard (`shared::period::require_open`) looks a posting date up here.
CREATE INDEX financial_period_dates_idx
    ON finance.financial_period (tenant_id, institution_id, start_date, end_date);

-- ------------------------------------------------------------------ manual journals
CREATE TABLE finance.manual_journal (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    journal_number       text           NOT NULL,
    posting_date         date           NOT NULL,
    -- `manual` (correction / reclassification) or `adjusting` (period-end accrual) or `opening`.
    entry_kind           text           NOT NULL DEFAULT 'manual'
                                        CHECK (entry_kind IN ('manual', 'adjusting', 'opening')),
    description          text           NOT NULL CHECK (length(btrim(description)) > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    total_amount         numeric(18,2)  NOT NULL CHECK (total_amount > 0),
    status               text           NOT NULL DEFAULT 'draft'
                                        CHECK (status IN ('draft', 'pending_approval', 'approved', 'posted', 'rejected', 'reversed')),
    status_reason        text,
    process_instance_id  text,
    requested_by         text,
    submitted_at         timestamptz,
    decided_by           text,
    decided_at           timestamptz,
    decision_reason      text,
    journal_entry_id     uuid           REFERENCES finance.journal_entry (id),
    reversal_entry_id    uuid           REFERENCES finance.journal_entry (id),
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX manual_journal_number_per_institution
    ON finance.manual_journal (tenant_id, institution_id, journal_number);
CREATE INDEX manual_journal_status_idx
    ON finance.manual_journal (tenant_id, institution_id, status, posting_date);

-- Lines are editable while the journal is a draft (replaced as a whole); the ledger copy is the
-- immutable record once posted.
CREATE TABLE finance.manual_journal_line (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    manual_journal_id    uuid           NOT NULL REFERENCES finance.manual_journal (id),
    line_no              integer        NOT NULL CHECK (line_no > 0),
    ledger_account_id    uuid           NOT NULL REFERENCES finance.ledger_account (id),
    debit                numeric(18,2)  NOT NULL DEFAULT 0 CHECK (debit >= 0),
    credit               numeric(18,2)  NOT NULL DEFAULT 0 CHECK (credit >= 0),
    student_account_id   uuid           REFERENCES finance.student_account (id),
    memo                 text,
    CHECK ((debit > 0) <> (credit > 0)),
    UNIQUE (manual_journal_id, line_no)
);

-- ------------------------------------------------------------------ reconciliation + trial balance
CREATE TABLE finance.ledger_reconciliation (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    -- `receivables`: Σ charge outstanding (and per charge) ↔ the receivable control account.
    scope                text           NOT NULL DEFAULT 'receivables' CHECK (scope IN ('receivables')),
    as_of                date           NOT NULL,
    status               text           NOT NULL CHECK (status IN ('open', 'matched', 'exceptions', 'resolved', 'closed')),
    source_total         numeric(18,2)  NOT NULL,
    ledger_total         numeric(18,2)  NOT NULL,
    difference           numeric(18,2)  NOT NULL,
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    exceptions           jsonb          NOT NULL DEFAULT '[]'::jsonb,
    performed_by         text,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE INDEX ledger_reconciliation_idx ON finance.ledger_reconciliation (tenant_id, institution_id, created_at);

-- Snapshot taken by the controlled accounting close (append-only evidence).
CREATE TABLE finance.trial_balance (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    financial_period_id  uuid           REFERENCES finance.financial_period (id),
    as_of                date           NOT NULL,
    total_debit          numeric(18,2)  NOT NULL,
    total_credit         numeric(18,2)  NOT NULL,
    balanced             boolean        NOT NULL,
    rows                 jsonb          NOT NULL,
    generated_by         text,
    created_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE INDEX trial_balance_idx ON finance.trial_balance (tenant_id, institution_id, as_of);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.trial_balance FROM sutra_finance;
CREATE TRIGGER trial_balance_append_only BEFORE UPDATE OR DELETE ON finance.trial_balance
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.financial_year, finance.financial_period,
    finance.manual_journal, finance.manual_journal_line, finance.ledger_reconciliation TO sutra_finance;
GRANT SELECT, INSERT ON finance.trial_balance TO sutra_finance;
