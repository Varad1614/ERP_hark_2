-- ERP_EXTENSIONS 1B — bank reconciliation against the statement balance (reconciled 'R' entries,
-- carried-forward balance, undo last, changes after reconciliation, BRS). Logic ported from
-- HeyPuter/everything accounting reconcile.js / banking.js / reports.js (MIT); see THIRD_PARTY_NOTICES.md.

-- One finished (or undone) reconciliation of a bank account up to a statement date. Reconciled
-- entries are derived from these records; the journal itself is never rewritten.
CREATE TABLE finance.bank_account_reconciliation (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    bank_account_id      uuid           NOT NULL REFERENCES finance.bank_account (id),
    ledger_account_id    uuid           NOT NULL REFERENCES finance.ledger_account (id),
    statement_date       date           NOT NULL,
    beginning_balance    numeric(18,2)  NOT NULL,
    ending_balance       numeric(18,2)  NOT NULL,
    cleared_count        integer        NOT NULL CHECK (cleared_count >= 0),
    status               text           NOT NULL DEFAULT 'done' CHECK (status IN ('done', 'undone')),
    note                 text,
    finished_by          text,
    finished_at          timestamptz    NOT NULL DEFAULT clock_timestamp(),
    undone_by            text,
    undone_at            timestamptz,
    CHECK ((status = 'undone') = (undone_at IS NOT NULL))
);
CREATE INDEX bank_account_reconciliation_account_idx
    ON finance.bank_account_reconciliation (tenant_id, bank_account_id, status, statement_date DESC, finished_at DESC);

-- The entries a reconciliation cleared, with the amount each had on the bank's ledger account
-- (debit positive) at that moment.
CREATE TABLE finance.bank_account_reconciliation_entry (
    reconciliation_id    uuid           NOT NULL REFERENCES finance.bank_account_reconciliation (id) ON DELETE CASCADE,
    tenant_id            uuid           NOT NULL,
    journal_entry_id     uuid           NOT NULL REFERENCES finance.journal_entry (id),
    amount               numeric(18,2)  NOT NULL,
    PRIMARY KEY (reconciliation_id, journal_entry_id)
);
CREATE INDEX bank_account_reconciliation_entry_entry_idx
    ON finance.bank_account_reconciliation_entry (tenant_id, journal_entry_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.bank_account_reconciliation,
    finance.bank_account_reconciliation_entry TO sutra_finance;

-- Forced row-level security on the new tenant tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
