-- Batch 5 slice 5B — refunds (L2-F03, L3-F-014 … F-019). Refund approval is the reference
-- approval of Batch 5 (requested → under_review → approved | rejected, WorkflowPort).

CREATE TABLE finance.refund (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    refund_number        text           NOT NULL,
    -- The payment refunded to (gateway refunds go back to the original instrument).
    payment_id           uuid           REFERENCES finance.payment (id),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    currency             text           NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    reason               text           NOT NULL CHECK (length(btrim(reason)) > 0),
    method               text           NOT NULL CHECK (method IN ('original', 'bank_transfer')),
    status               text           NOT NULL CHECK (status IN ('requested', 'under_review', 'approved',
                                            'rejected', 'processing', 'completed', 'failed', 'cancelled',
                                            'reversed', 'closed')),
    status_reason        text,
    requested_via        text           NOT NULL CHECK (requested_via IN ('self', 'staff')),
    requested_by         text,
    -- Unapplied credit consumed at approval: [{"paymentId": uuid, "amount": "12.00"}].
    sources              jsonb          NOT NULL DEFAULT '[]',
    process_instance_id  text,
    decided_by           text,
    decided_at           timestamptz,
    decision_reason      text,
    approve_entry_id     uuid           REFERENCES finance.journal_entry (id),
    pay_entry_id         uuid           REFERENCES finance.journal_entry (id),
    gateway_refund_ref   text,
    attempts             integer        NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    failure_reason       text,
    completed_at         timestamptz,
    closed_at            timestamptz,
    version              bigint         NOT NULL DEFAULT 1,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    updated_at           timestamptz    NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX refund_number_per_institution ON finance.refund (tenant_id, institution_id, refund_number);
CREATE INDEX refund_account_idx ON finance.refund (tenant_id, student_account_id, created_at);
CREATE INDEX refund_institution_idx ON finance.refund (tenant_id, institution_id, status, created_at);
