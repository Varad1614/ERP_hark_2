-- Batch 5 slice 5B — accounts receivable, dunning, write-off decisions (L2-F07,
-- L3-F-039 … F-043). One receivable per published charge.

CREATE TABLE finance.receivable (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    charge_id            uuid           NOT NULL REFERENCES finance.charge (id),
    -- Amount recognised (charge total + adjustments at recognition) and the last evaluated outstanding.
    recognised_amount    numeric(18,2)  NOT NULL CHECK (recognised_amount >= 0),
    outstanding_amount   numeric(18,2)  NOT NULL CHECK (outstanding_amount >= 0),
    written_off_amount   numeric(18,2)  NOT NULL DEFAULT 0 CHECK (written_off_amount >= 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    due_date             date,
    status               text           NOT NULL CHECK (status IN ('open', 'partially_settled', 'overdue',
                                            'settled', 'written_off', 'reopened')),
    status_reason        text,
    recognised_at        timestamptz    NOT NULL DEFAULT now(),
    evaluated_on         date,
    process_instance_id  text,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, charge_id)
);
CREATE INDEX receivable_institution_idx ON finance.receivable (tenant_id, institution_id, status);
CREATE INDEX receivable_account_idx ON finance.receivable (tenant_id, student_account_id);

CREATE TABLE finance.dunning_case (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    receivable_id        uuid           NOT NULL REFERENCES finance.receivable (id),
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    status               text           NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'notice_sent',
                                            'escalated', 'resolved', 'cancelled')),
    level                integer        NOT NULL DEFAULT 0 CHECK (level >= 0),
    status_reason        text,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
-- At most one live case per receivable.
CREATE UNIQUE INDEX dunning_case_live_unique ON finance.dunning_case (tenant_id, receivable_id)
    WHERE status IN ('open', 'notice_sent', 'escalated');

CREATE TABLE finance.dunning_notice (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    dunning_case_id      uuid           NOT NULL REFERENCES finance.dunning_case (id),
    level                integer        NOT NULL CHECK (level > 0),
    amount_due           numeric(18,2)  NOT NULL CHECK (amount_due >= 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    notice_date          date           NOT NULL,
    message              text           NOT NULL,
    sent_by              text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (dunning_case_id, level)
);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.dunning_notice FROM sutra_finance;
CREATE TRIGGER dunning_notice_append_only BEFORE UPDATE OR DELETE ON finance.dunning_notice
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

CREATE TABLE finance.receivable_write_off (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    receivable_id        uuid           NOT NULL REFERENCES finance.receivable (id),
    charge_id            uuid           NOT NULL REFERENCES finance.charge (id),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    reason               text           NOT NULL CHECK (length(btrim(reason)) > 0),
    status               text           NOT NULL CHECK (status IN ('pending_approval', 'posted',
                                            'rejected', 'reversed')),
    process_instance_id  text,
    requested_by         text,
    decided_by           text,
    decided_at           timestamptz,
    decision_reason      text,
    journal_entry_id     uuid           REFERENCES finance.journal_entry (id),
    reversal_entry_id    uuid           REFERENCES finance.journal_entry (id),
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
-- At most one write-off awaiting a decision or posted per receivable.
CREATE UNIQUE INDEX receivable_write_off_live_unique ON finance.receivable_write_off (tenant_id, receivable_id)
    WHERE status IN ('pending_approval', 'posted');
