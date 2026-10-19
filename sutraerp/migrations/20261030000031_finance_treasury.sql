-- Batch 5 slice 5C — treasury (L2-F10, L3-F-056…061): bank accounts (each mapped to a postable
-- asset ledger account), bank transactions (gateway settlements and other bank movements), bank
-- statement import (JSON lines) and statement reconciliation.

CREATE TABLE finance.bank_account (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    name                 text           NOT NULL CHECK (length(btrim(name)) > 0),
    bank_name            text           NOT NULL CHECK (length(btrim(bank_name)) > 0),
    -- Only the last four digits are kept (the full number lives with the bank).
    account_number_last4 text           NOT NULL CHECK (account_number_last4 ~ '^[0-9A-Z]{4}$'),
    ifsc                 text           NOT NULL CHECK (ifsc ~ '^[A-Z]{4}0[A-Z0-9]{6}$'),
    purpose              text           NOT NULL DEFAULT 'collection'
                                        CHECK (purpose IN ('collection', 'operating', 'payout', 'deposit')),
    ledger_account_id    uuid           NOT NULL REFERENCES finance.ledger_account (id),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status               text           NOT NULL DEFAULT 'requested'
                                        CHECK (status IN ('requested', 'approved', 'active', 'suspended', 'closed')),
    status_reason        text,
    process_instance_id  text,
    requested_by         text,
    decided_by           text,
    decided_at           timestamptz,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
-- One bank account per ledger account (the bank ledger is the reconciliation target).
CREATE UNIQUE INDEX bank_account_ledger_account ON finance.bank_account (ledger_account_id);
CREATE UNIQUE INDEX bank_account_number_per_institution
    ON finance.bank_account (tenant_id, institution_id, ifsc, account_number_last4, name);

CREATE TABLE finance.bank_transaction (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    bank_account_id      uuid           NOT NULL REFERENCES finance.bank_account (id),
    -- `gateway_settlement`: the gateway paid captured payments out to the bank (Dr bank + bank
    -- charges · Cr gateway clearing, one `payment` / `settle` entry per payment).
    kind                 text           NOT NULL CHECK (kind IN ('gateway_settlement')),
    direction            text           NOT NULL CHECK (direction IN ('credit', 'debit')),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    fee_amount           numeric(18,2)  NOT NULL DEFAULT 0 CHECK (fee_amount >= 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    value_date           date           NOT NULL,
    reference            text           NOT NULL CHECK (length(btrim(reference)) > 0),
    status               text           NOT NULL DEFAULT 'executed'
                                        CHECK (status IN ('pending', 'executed', 'failed', 'returned', 'uncertain', 'resolved')),
    status_reason        text,
    -- `[{paymentId, gross, fee, net, journalEntryId}]`
    items                jsonb          NOT NULL DEFAULT '[]'::jsonb,
    created_by           text,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX bank_transaction_reference
    ON finance.bank_transaction (tenant_id, bank_account_id, kind, reference);

-- A gateway payment is settled to the bank at most once (5B's `payment` id, plain uuid).
CREATE TABLE finance.bank_transaction_item (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    bank_transaction_id  uuid           NOT NULL REFERENCES finance.bank_transaction (id),
    payment_id           uuid           NOT NULL,
    gross_amount         numeric(18,2)  NOT NULL CHECK (gross_amount > 0),
    fee_amount           numeric(18,2)  NOT NULL DEFAULT 0 CHECK (fee_amount >= 0),
    journal_entry_id     uuid           NOT NULL REFERENCES finance.journal_entry (id),
    CHECK (fee_amount < gross_amount)
);
CREATE UNIQUE INDEX bank_transaction_item_payment ON finance.bank_transaction_item (tenant_id, payment_id);

CREATE TABLE finance.bank_statement (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    bank_account_id      uuid           NOT NULL REFERENCES finance.bank_account (id),
    statement_ref        text           NOT NULL CHECK (length(btrim(statement_ref)) > 0),
    period_from          date           NOT NULL,
    period_to            date           NOT NULL,
    opening_balance      numeric(18,2),
    closing_balance      numeric(18,2),
    total_credits        numeric(18,2)  NOT NULL DEFAULT 0,
    total_debits         numeric(18,2)  NOT NULL DEFAULT 0,
    line_count           integer        NOT NULL CHECK (line_count > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    -- sha256 of the imported content: the same file is never imported twice.
    content_sha256       text           NOT NULL,
    status               text           NOT NULL DEFAULT 'imported'
                                        CHECK (status IN ('imported', 'reconciling', 'reconciled', 'failed')),
    imported_by          text,
    reconciled_at        timestamptz,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    CHECK (period_to >= period_from)
);
CREATE UNIQUE INDEX bank_statement_ref_per_account ON finance.bank_statement (bank_account_id, statement_ref);
CREATE UNIQUE INDEX bank_statement_content_per_account ON finance.bank_statement (bank_account_id, content_sha256);

CREATE TABLE finance.bank_statement_line (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    bank_statement_id    uuid           NOT NULL REFERENCES finance.bank_statement (id),
    line_no              integer        NOT NULL CHECK (line_no > 0),
    value_date           date           NOT NULL,
    -- `credit` = money into the account, `debit` = money out (bank's view of the account).
    direction            text           NOT NULL CHECK (direction IN ('credit', 'debit')),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    reference            text,
    description          text,
    match_status         text           NOT NULL DEFAULT 'unmatched'
                                        CHECK (match_status IN ('unmatched', 'matched', 'ignored')),
    matched_journal_line_id   uuid      REFERENCES finance.journal_line (id),
    matched_transaction_id    uuid      REFERENCES finance.bank_transaction (id),
    matched_by           text,
    note                 text,
    UNIQUE (bank_statement_id, line_no),
    CHECK (match_status <> 'matched' OR (matched_journal_line_id IS NOT NULL) <> (matched_transaction_id IS NOT NULL))
);
-- A ledger line / bank transaction explains at most one statement line.
CREATE UNIQUE INDEX bank_statement_line_journal_match
    ON finance.bank_statement_line (matched_journal_line_id) WHERE matched_journal_line_id IS NOT NULL;
CREATE UNIQUE INDEX bank_statement_line_transaction_match
    ON finance.bank_statement_line (matched_transaction_id) WHERE matched_transaction_id IS NOT NULL;

CREATE TABLE finance.bank_reconciliation (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    bank_statement_id    uuid           NOT NULL REFERENCES finance.bank_statement (id),
    status               text           NOT NULL CHECK (status IN ('open', 'matched', 'exceptions', 'resolved', 'closed')),
    matched_lines        integer        NOT NULL,
    unmatched_lines      integer        NOT NULL,
    ignored_lines        integer        NOT NULL,
    statement_closing    numeric(18,2),
    ledger_balance       numeric(18,2)  NOT NULL,
    performed_by         text,
    created_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE INDEX bank_reconciliation_statement_idx ON finance.bank_reconciliation (bank_statement_id, created_at);

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.bank_account, finance.bank_transaction,
    finance.bank_transaction_item, finance.bank_statement, finance.bank_statement_line,
    finance.bank_reconciliation TO sutra_finance;
