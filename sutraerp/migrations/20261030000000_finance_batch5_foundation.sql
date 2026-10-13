-- Batch 5 foundation — Domain F (Finance) shared tables (docs/batch5-contracts.md §3).
--
-- Foundation-owned. Slices 5A/5B/5C add their own tables in their migration ranges
-- (5A 20261030000001-009, 5B …010-029, 5C …030-039) and never edit this file.
--
-- Money: every amount is NUMERIC(18,2) with a `currency` column (INR in Batch 5); Rust uses
-- rust_decimal end to end (never floats). Amounts on the wire are JSON strings.
--
-- Write rules (§3.3):
--   ledger_account          COA seed by `shared::coa::ensure_default_chart` (foundation); 5C manages
--                           accounts afterwards (create / deactivate).
--   fee_head                5A.
--   student_account         `shared::account::ensure_*` only (5A on publication, 5B for applicant /
--                           unidentified payers).
--   charge, charge_line     5A creates; status ONLY via `shared::lifecycle::change_charge_status`;
--                           `adjustment_amount` ONLY via `shared::charge::apply_adjustment` (5A);
--                           `settled_amount` ONLY via `shared::charge::apply_settlement` (5B).
--   journal_entry/_line     ONLY via `shared::posting::{post, reverse}` (append-only, balanced,
--                           period-guarded). 5C builds batches / manual journals / close on top.
--   number_sequence         `shared::numbering::next` only.
--   status_history          `shared::lifecycle` only (append-only).

-- Append-only guard reused by every finance history / ledger table (slices reuse it too).
CREATE FUNCTION finance.forbid_history_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'rows of finance.% are append-only (no physical deletion or rewrite)', TG_TABLE_NAME;
END;
$$;

-- ------------------------------------------------------------------ chart of accounts
CREATE TABLE finance.ledger_account (
    id               uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid          NOT NULL,
    institution_id   uuid          NOT NULL,
    code             text          NOT NULL CHECK (code ~ '^[0-9A-Z][0-9A-Z._-]{0,31}$'),
    name             text          NOT NULL CHECK (length(btrim(name)) > 0),
    account_type     text          NOT NULL CHECK (account_type IN ('asset', 'liability', 'equity', 'income', 'expense')),
    normal_balance   text          NOT NULL CHECK (normal_balance IN ('debit', 'credit')),
    parent_id        uuid          REFERENCES finance.ledger_account (id),
    -- Stable role of a seeded account (`shared::coa::SystemAccount`); NULL for user accounts.
    system_key       text,
    is_postable      boolean       NOT NULL DEFAULT true,
    currency         text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status           text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive')),
    version          bigint        NOT NULL DEFAULT 1,
    created_at       timestamptz   NOT NULL DEFAULT now(),
    updated_at       timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ledger_account_code_per_institution
    ON finance.ledger_account (tenant_id, institution_id, code);
CREATE UNIQUE INDEX ledger_account_system_key_per_institution
    ON finance.ledger_account (tenant_id, institution_id, system_key) WHERE system_key IS NOT NULL;

-- ------------------------------------------------------------------ fee heads
CREATE TABLE finance.fee_head (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    code               text          NOT NULL CHECK (code ~ '^[A-Z0-9][A-Z0-9_-]{1,31}$'),
    name               text          NOT NULL CHECK (length(btrim(name)) > 0),
    category           text          NOT NULL CHECK (category IN ('tuition', 'admission', 'examination',
                                         'hostel', 'transport', 'library', 'laboratory', 'fine',
                                         'deposit', 'miscellaneous')),
    income_account_id  uuid          NOT NULL REFERENCES finance.ledger_account (id),
    refundable         boolean       NOT NULL DEFAULT false,
    status             text          NOT NULL DEFAULT 'active' CHECK (status IN ('draft', 'active', 'retired')),
    version            bigint        NOT NULL DEFAULT 1,
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX fee_head_code_per_institution
    ON finance.fee_head (tenant_id, institution_id, code);

-- ------------------------------------------------------------------ party (student finance account)
CREATE TABLE finance.student_account (
    id                  uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid          NOT NULL,
    institution_id      uuid          NOT NULL,
    party_kind          text          NOT NULL CHECK (party_kind IN ('student', 'applicant')),
    -- Domain C ids (plain, no FK into `student`): a student, or a pre-student admission
    -- (L2-F01 admission charge context amendment 2026-10-05).
    student_id          uuid,
    admission_id        uuid,
    -- Snapshots for lists, receipts and notifications (no join into `student`).
    student_number      text,
    display_name        text          NOT NULL CHECK (length(btrim(display_name)) > 0),
    subject_ref         text,
    currency            text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status              text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'on_hold', 'closed')),
    version             bigint        NOT NULL DEFAULT 1,
    created_at          timestamptz   NOT NULL DEFAULT now(),
    updated_at          timestamptz   NOT NULL DEFAULT now(),
    CHECK ((party_kind = 'student' AND student_id IS NOT NULL)
        OR (party_kind = 'applicant' AND admission_id IS NOT NULL))
);
CREATE UNIQUE INDEX student_account_one_per_student
    ON finance.student_account (tenant_id, institution_id, student_id) WHERE student_id IS NOT NULL;
CREATE UNIQUE INDEX student_account_one_per_admission
    ON finance.student_account (tenant_id, institution_id, admission_id)
    WHERE admission_id IS NOT NULL AND student_id IS NULL;
CREATE INDEX student_account_subject_idx ON finance.student_account (tenant_id, subject_ref);

-- ------------------------------------------------------------------ charges (obligations)
CREATE TABLE finance.charge (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    student_id           uuid,
    admission_id         uuid,
    charge_number        text           NOT NULL,
    academic_year_id     uuid,
    academic_term_id     uuid,
    description          text           NOT NULL CHECK (length(btrim(description)) > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    total_amount         numeric(18,2)  NOT NULL CHECK (total_amount > 0),
    -- Net of posted credit (-) / debit (+) notes, write-offs, waivers, aid (5A, apply_adjustment).
    adjustment_amount    numeric(18,2)  NOT NULL DEFAULT 0,
    -- Allocated receipts net of reversals (5B, apply_settlement).
    settled_amount       numeric(18,2)  NOT NULL DEFAULT 0 CHECK (settled_amount >= 0),
    outstanding_amount   numeric(18,2)  GENERATED ALWAYS AS (total_amount + adjustment_amount - settled_amount) STORED,
    due_date             date,
    source_type          text           NOT NULL CHECK (source_type IN ('fee_structure', 'manual',
                                            'admission', 'debit_note', 'aid_recovery', 'fine')),
    source_id            uuid,
    status               text           NOT NULL CHECK (status IN ('draft', 'pending_validation',
                                            'validated', 'published', 'active', 'suspended', 'disputed',
                                            'cancelled', 'waived', 'written_off', 'closed', 'archived')),
    status_reason        text,
    status_changed_at    timestamptz    NOT NULL DEFAULT now(),
    process_instance_id  text,
    published_at         timestamptz,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    CHECK (total_amount + adjustment_amount >= 0),
    CHECK (settled_amount <= total_amount + adjustment_amount)
);
CREATE UNIQUE INDEX charge_number_per_institution
    ON finance.charge (tenant_id, institution_id, charge_number);
CREATE INDEX charge_account_idx ON finance.charge (tenant_id, student_account_id, status);
CREATE INDEX charge_student_idx ON finance.charge (tenant_id, institution_id, student_id);
CREATE INDEX charge_admission_idx ON finance.charge (tenant_id, institution_id, admission_id);

CREATE TABLE finance.charge_line (
    id                 uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid           NOT NULL,
    charge_id          uuid           NOT NULL REFERENCES finance.charge (id),
    line_no            integer        NOT NULL CHECK (line_no > 0),
    fee_head_id        uuid           NOT NULL REFERENCES finance.fee_head (id),
    description        text           NOT NULL,
    amount             numeric(18,2)  NOT NULL CHECK (amount > 0),
    -- Snapshot of the fee head's income account at generation.
    income_account_id  uuid           NOT NULL REFERENCES finance.ledger_account (id),
    created_at         timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (charge_id, line_no)
);

-- Archived charges are immutable; no physical deletion of charges or their lines.
CREATE FUNCTION finance.forbid_archived_charge_change() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.status = 'archived' THEN
        RAISE EXCEPTION 'charge % is archived and immutable', OLD.id;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER charge_archived_immutable BEFORE UPDATE ON finance.charge
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_archived_charge_change();
CREATE TRIGGER charge_no_delete BEFORE DELETE ON finance.charge
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();
CREATE TRIGGER charge_line_no_change BEFORE UPDATE OR DELETE ON finance.charge_line
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- ------------------------------------------------------------------ the general ledger (append-only)
CREATE TABLE finance.journal_entry (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    entry_number         text           NOT NULL,
    posting_date         date           NOT NULL,
    -- 5C's financial period (plain uuid, filled by the period guard once 5C lands).
    financial_period_id  uuid,
    kind                 text           NOT NULL CHECK (kind IN ('standard', 'reversal', 'manual',
                                            'adjusting', 'opening', 'closing')),
    -- What caused the entry: e.g. `charge` / `adjustment` / `payment` / `refund` / `manual_journal`.
    source_type          text           NOT NULL CHECK (source_type ~ '^[a-z][a-z0-9_]{1,47}$'),
    source_id            uuid           NOT NULL,
    -- Distinguishes several postings of one source (`publish`, `settle:<allocation>`, …).
    posting_key          text           NOT NULL CHECK (length(posting_key) BETWEEN 1 AND 120),
    description          text           NOT NULL,
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    total_debit          numeric(18,2)  NOT NULL CHECK (total_debit > 0),
    total_credit         numeric(18,2)  NOT NULL CHECK (total_credit > 0),
    reverses_entry_id    uuid           REFERENCES finance.journal_entry (id),
    posted_by            text,
    correlation_id       text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    CHECK (total_debit = total_credit),
    CHECK ((kind = 'reversal') = (reverses_entry_id IS NOT NULL))
);
-- Idempotent posting: one entry per (source, posting key).
CREATE UNIQUE INDEX journal_entry_source_key
    ON finance.journal_entry (tenant_id, source_type, source_id, posting_key);
CREATE UNIQUE INDEX journal_entry_number_per_institution
    ON finance.journal_entry (tenant_id, institution_id, entry_number);
-- An entry is reversed at most once.
CREATE UNIQUE INDEX journal_entry_reversed_once
    ON finance.journal_entry (reverses_entry_id) WHERE reverses_entry_id IS NOT NULL;
CREATE INDEX journal_entry_date_idx ON finance.journal_entry (tenant_id, institution_id, posting_date);

CREATE TABLE finance.journal_line (
    id                  uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid           NOT NULL,
    journal_entry_id    uuid           NOT NULL REFERENCES finance.journal_entry (id),
    line_no             integer        NOT NULL CHECK (line_no > 0),
    ledger_account_id   uuid           NOT NULL REFERENCES finance.ledger_account (id),
    debit               numeric(18,2)  NOT NULL DEFAULT 0 CHECK (debit >= 0),
    credit              numeric(18,2)  NOT NULL DEFAULT 0 CHECK (credit >= 0),
    -- Sub-ledger dimensions (receivables by party / charge).
    student_account_id  uuid           REFERENCES finance.student_account (id),
    charge_id           uuid           REFERENCES finance.charge (id),
    memo                text,
    created_at          timestamptz    NOT NULL DEFAULT now(),
    CHECK ((debit > 0) <> (credit > 0)),
    UNIQUE (journal_entry_id, line_no)
);
CREATE INDEX journal_line_account_idx ON finance.journal_line (tenant_id, ledger_account_id);
CREATE INDEX journal_line_party_idx ON finance.journal_line (tenant_id, student_account_id);

CREATE TRIGGER journal_entry_append_only BEFORE UPDATE OR DELETE ON finance.journal_entry
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();
CREATE TRIGGER journal_line_append_only BEFORE UPDATE OR DELETE ON finance.journal_line
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- Double entry, enforced by the database at commit: the lines of an entry balance and match the
-- header totals (an entry with fewer than two lines fails too).
CREATE FUNCTION finance.assert_journal_balanced() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    d numeric(18,2);
    c numeric(18,2);
    n integer;
    hd numeric(18,2);
    hc numeric(18,2);
    eid uuid;
BEGIN
    IF TG_TABLE_NAME = 'journal_entry' THEN
        eid := NEW.id;
    ELSE
        eid := NEW.journal_entry_id;
    END IF;
    SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0), count(*)
      INTO d, c, n FROM finance.journal_line WHERE journal_entry_id = eid;
    SELECT total_debit, total_credit INTO hd, hc FROM finance.journal_entry WHERE id = eid;
    IF n < 2 OR d <> c OR d <> hd OR c <> hc THEN
        RAISE EXCEPTION 'journal entry % is unbalanced (lines: % debit / % credit over % lines; header % / %)',
            eid, d, c, n, hd, hc USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER journal_line_balanced AFTER INSERT ON finance.journal_line
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION finance.assert_journal_balanced();
CREATE CONSTRAINT TRIGGER journal_entry_balanced AFTER INSERT ON finance.journal_entry
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION finance.assert_journal_balanced();

-- ------------------------------------------------------------------ numbering
CREATE TABLE finance.number_sequence (
    tenant_id       uuid     NOT NULL,
    institution_id  uuid     NOT NULL,
    -- `charge` (CHG), `journal` (JE), `receipt` (RCT), `refund` (RFD), `adjustment` (ADJ), … (§3.4)
    kind            text     NOT NULL CHECK (kind ~ '^[a-z][a-z_]{1,31}$'),
    next_value      bigint   NOT NULL DEFAULT 1 CHECK (next_value > 0),
    PRIMARY KEY (tenant_id, institution_id, kind)
);

-- ------------------------------------------------------------------ status history (append-only)
CREATE TABLE finance.status_history (
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
CREATE INDEX finance_status_history_aggregate_idx
    ON finance.status_history (tenant_id, aggregate_type, aggregate_id, created_at);
CREATE TRIGGER finance_status_history_append_only BEFORE UPDATE OR DELETE ON finance.status_history
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- ------------------------------------------------------------------ runtime role (sutra_finance)
-- The role and the schema default privileges come from 20261014000003_runtime_db_roles.sql; the
-- grants are restated here so the foundation tables are covered on clusters provisioned out of
-- band, and the ledger + history tables are append-only at the privilege level as well.
GRANT USAGE ON SCHEMA finance TO sutra_finance;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA finance TO sutra_finance;
REVOKE UPDATE, DELETE, TRUNCATE ON finance.journal_entry, finance.journal_line,
    finance.status_history, finance.charge_line FROM sutra_finance;
GRANT INSERT ON platform.outbox, platform.audit_log TO sutra_finance;
