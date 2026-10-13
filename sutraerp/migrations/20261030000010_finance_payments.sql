-- Batch 5 slice 5B — payments, allocations, receipts, gateway webhook log, unidentified receipts,
-- bulk collections (L2-F02, L3-F-007 … F-013; docs/batch5-contracts.md §3).
-- Money: numeric(18,2) + currency INR; no floats. Ledger postings go through shared::posting only.

-- ------------------------------------------------------------------ bulk collection batches (F-013)
CREATE TABLE finance.payment_batch (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    name                 text           NOT NULL CHECK (length(btrim(name)) > 0),
    channel              text           NOT NULL CHECK (channel IN ('cash', 'cheque', 'bank_transfer',
                                            'demand_draft', 'sponsor')),
    received_on          date           NOT NULL,
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    total_amount         numeric(18,2)  NOT NULL DEFAULT 0 CHECK (total_amount >= 0),
    status               text           NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'validated',
                                            'executing', 'completed', 'partially_completed', 'failed')),
    process_instance_id  text,
    created_by           text,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE INDEX payment_batch_institution_idx ON finance.payment_batch (tenant_id, institution_id, created_at);

-- ------------------------------------------------------------------ payments (F-007 … F-012)
CREATE TABLE finance.payment (
    id                    uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid           NOT NULL,
    institution_id        uuid           NOT NULL,
    student_account_id    uuid           NOT NULL REFERENCES finance.student_account (id),
    payment_number        text           NOT NULL,
    channel               text           NOT NULL CHECK (channel IN ('gateway', 'cash', 'cheque',
                                             'bank_transfer', 'demand_draft', 'sponsor')),
    payer_kind            text           NOT NULL DEFAULT 'student' CHECK (payer_kind IN ('student', 'sponsor')),
    payer_name            text,
    external_ref          text,
    amount                numeric(18,2)  NOT NULL CHECK (amount > 0),
    currency              text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    -- Part of a completed payment not (yet) allocated to charges (Cr unapplied receipts).
    unapplied_amount      numeric(18,2)  NOT NULL DEFAULT 0 CHECK (unapplied_amount >= 0),
    status                text           NOT NULL CHECK (status IN ('initiated', 'pending', 'completed',
                                             'failed', 'expired', 'cancelled', 'reversed', 'charged_back')),
    status_reason         text,
    -- Charges the payer asked to pay (allocation targets, oldest due first when empty).
    target_charge_ids     uuid[]         NOT NULL DEFAULT '{}',
    gateway               text,
    gateway_order_ref     text,
    gateway_payment_ref   text,
    checkout_url          text,
    failure_code          text,
    idempotency_key       text,
    received_on           date           NOT NULL,
    completed_at          timestamptz,
    journal_entry_id      uuid           REFERENCES finance.journal_entry (id),
    reversal_entry_id     uuid           REFERENCES finance.journal_entry (id),
    process_instance_id   text,
    unidentified_receipt_id uuid,
    batch_id              uuid           REFERENCES finance.payment_batch (id),
    created_by            text,
    version               bigint         NOT NULL DEFAULT 1,
    created_at            timestamptz    NOT NULL DEFAULT now(),
    updated_at            timestamptz    NOT NULL DEFAULT now(),
    CHECK (unapplied_amount <= amount),
    CHECK (channel <> 'gateway' OR gateway_order_ref IS NOT NULL)
);
CREATE UNIQUE INDEX payment_number_per_institution ON finance.payment (tenant_id, institution_id, payment_number);
CREATE UNIQUE INDEX payment_gateway_order_unique ON finance.payment (gateway, gateway_order_ref)
    WHERE gateway_order_ref IS NOT NULL;
CREATE UNIQUE INDEX payment_idempotency_unique ON finance.payment (tenant_id, student_account_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX payment_account_idx ON finance.payment (tenant_id, student_account_id, created_at);
CREATE INDEX payment_institution_idx ON finance.payment (tenant_id, institution_id, status, created_at);

-- ------------------------------------------------------------------ allocations (F-008)
CREATE TABLE finance.payment_allocation (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    payment_id           uuid           NOT NULL REFERENCES finance.payment (id),
    charge_id            uuid           NOT NULL REFERENCES finance.charge (id),
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    -- `capture`: allocated with the receipt; `unapplied`: allocated later from the unapplied part.
    source               text           NOT NULL CHECK (source IN ('capture', 'unapplied')),
    status               text           NOT NULL DEFAULT 'allocated' CHECK (status IN ('allocated', 'reversed')),
    journal_entry_id     uuid           REFERENCES finance.journal_entry (id),
    allocated_by         text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    reversed_at          timestamptz
);
CREATE INDEX payment_allocation_payment_idx ON finance.payment_allocation (tenant_id, payment_id);
CREATE INDEX payment_allocation_charge_idx ON finance.payment_allocation (tenant_id, charge_id);

-- ------------------------------------------------------------------ receipts (F-007)
CREATE TABLE finance.receipt (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    payment_id           uuid           NOT NULL UNIQUE REFERENCES finance.payment (id),
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    receipt_number       text           NOT NULL,
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    status               text           NOT NULL DEFAULT 'issued' CHECK (status IN ('issued', 'cancelled')),
    issued_at            timestamptz    NOT NULL DEFAULT now(),
    cancelled_at         timestamptz,
    cancel_reason        text,
    version              bigint         NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX receipt_number_per_institution ON finance.receipt (tenant_id, institution_id, receipt_number);
CREATE INDEX receipt_account_idx ON finance.receipt (tenant_id, student_account_id, issued_at);

-- ------------------------------------------------------------------ gateway webhook log (idempotency on the event id)
CREATE TABLE finance.gateway_webhook_event (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    gateway              text           NOT NULL,
    event_id             text           NOT NULL,
    event                text           NOT NULL,
    order_ref            text           NOT NULL,
    tenant_id            uuid,
    payment_id           uuid           REFERENCES finance.payment (id),
    outcome              text           NOT NULL,
    received_at          timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (gateway, event_id)
);
REVOKE UPDATE, DELETE, TRUNCATE ON finance.gateway_webhook_event FROM sutra_finance;
CREATE TRIGGER gateway_webhook_event_append_only BEFORE UPDATE OR DELETE ON finance.gateway_webhook_event
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

-- ------------------------------------------------------------------ unidentified receipts (F-009)
CREATE TABLE finance.unidentified_receipt (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    channel              text           NOT NULL CHECK (channel IN ('cash', 'cheque', 'bank_transfer',
                                            'demand_draft', 'sponsor')),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    received_on          date           NOT NULL,
    payer_name           text,
    external_ref         text,
    notes                text,
    status               text           NOT NULL DEFAULT 'unidentified' CHECK (status IN ('unidentified',
                                            'under_investigation', 'identified', 'allocated', 'refunded')),
    student_account_id   uuid           REFERENCES finance.student_account (id),
    payment_id           uuid           REFERENCES finance.payment (id),
    journal_entry_id     uuid           REFERENCES finance.journal_entry (id),
    resolution_entry_id  uuid           REFERENCES finance.journal_entry (id),
    created_by           text,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE INDEX unidentified_receipt_institution_idx ON finance.unidentified_receipt (tenant_id, institution_id, status);

-- ------------------------------------------------------------------ bulk collection items (F-013)
CREATE TABLE finance.payment_batch_item (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    batch_id             uuid           NOT NULL REFERENCES finance.payment_batch (id),
    line_no              integer        NOT NULL CHECK (line_no > 0),
    student_account_id   uuid           NOT NULL,
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    external_ref         text,
    status               text           NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'valid',
                                            'invalid', 'completed', 'failed')),
    error                text,
    payment_id           uuid           REFERENCES finance.payment (id),
    UNIQUE (batch_id, line_no)
);
