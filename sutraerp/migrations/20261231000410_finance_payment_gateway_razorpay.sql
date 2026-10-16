-- Part 2 · Stage 4 (S4-pay): Razorpay payment gateway.
--
-- 1. Recovery polling bookkeeping on gateway payments: the `payment_gateway_recovery` job asks the
--    gateway for the outcome of open (initiated / pending) payments and re-checks recently failed
--    ones for a late capture; `gateway_polled_at` / `gateway_poll_count` drive its exponential
--    back-off so a payment is not polled on every tick.
-- 2. Settlement reconciliation imports: a Razorpay settlement report (CSV, columns of
--    https://razorpay.com/docs/api/settlements/fetch-recon/) is matched line by line against
--    captured payments / processed refunds; every import and every line result is kept
--    (append-only) as the reconciliation audit trail. Importing the same file twice returns the
--    first import (unique file hash per institution and gateway).

ALTER TABLE finance.payment
    ADD COLUMN gateway_polled_at  timestamptz,
    ADD COLUMN gateway_poll_count integer NOT NULL DEFAULT 0 CHECK (gateway_poll_count >= 0);

CREATE INDEX payment_gateway_recovery_idx ON finance.payment (gateway, status, created_at)
    WHERE channel = 'gateway' AND status IN ('initiated', 'pending', 'failed');
CREATE INDEX payment_gateway_payment_ref_idx ON finance.payment (tenant_id, institution_id, gateway_payment_ref)
    WHERE gateway_payment_ref IS NOT NULL;

CREATE TABLE finance.gateway_settlement_import (
    id               uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid           NOT NULL,
    institution_id   uuid           NOT NULL,
    gateway          text           NOT NULL,
    file_sha256      text           NOT NULL CHECK (file_sha256 ~ '^[0-9a-f]{64}$'),
    file_name        text,
    amount_unit      text           NOT NULL CHECK (amount_unit IN ('rupees', 'paise')),
    line_count       integer        NOT NULL CHECK (line_count >= 0),
    matched_count    integer        NOT NULL CHECK (matched_count >= 0),
    mismatch_count   integer        NOT NULL CHECK (mismatch_count >= 0),
    -- Σ credit − Σ debit of the report (what the gateway paid out), Σ fee, Σ tax.
    settled_total    numeric(18,2)  NOT NULL DEFAULT 0,
    fee_total        numeric(18,2)  NOT NULL DEFAULT 0,
    tax_total        numeric(18,2)  NOT NULL DEFAULT 0,
    imported_by      text,
    imported_at      timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, institution_id, gateway, file_sha256)
);
CREATE INDEX gateway_settlement_import_institution_idx
    ON finance.gateway_settlement_import (tenant_id, institution_id, imported_at);

CREATE TABLE finance.gateway_settlement_line (
    id               uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid           NOT NULL,
    institution_id   uuid           NOT NULL,
    import_id        uuid           NOT NULL REFERENCES finance.gateway_settlement_import (id),
    line_no          integer        NOT NULL CHECK (line_no > 0),
    entity_id        text           NOT NULL,
    entity_type      text           NOT NULL,
    settlement_id    text,
    settlement_utr   text,
    order_id         text,
    amount           numeric(18,2)  NOT NULL DEFAULT 0,
    fee              numeric(18,2)  NOT NULL DEFAULT 0,
    tax              numeric(18,2)  NOT NULL DEFAULT 0,
    credit           numeric(18,2)  NOT NULL DEFAULT 0,
    debit            numeric(18,2)  NOT NULL DEFAULT 0,
    payment_id       uuid           REFERENCES finance.payment (id),
    refund_id        uuid           REFERENCES finance.refund (id),
    result           text           NOT NULL CHECK (result IN ('matched', 'amount_mismatch',
                                        'order_mismatch', 'unknown_payment', 'payment_not_completed',
                                        'already_settled', 'duplicate_line', 'refund_matched',
                                        'refund_amount_mismatch', 'unknown_refund', 'ignored')),
    detail           text,
    created_at       timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (import_id, line_no)
);
CREATE INDEX gateway_settlement_line_entity_idx
    ON finance.gateway_settlement_line (tenant_id, institution_id, entity_id);

REVOKE UPDATE, DELETE, TRUNCATE ON finance.gateway_settlement_import FROM sutra_finance;
REVOKE UPDATE, DELETE, TRUNCATE ON finance.gateway_settlement_line FROM sutra_finance;
CREATE TRIGGER gateway_settlement_import_append_only BEFORE UPDATE OR DELETE ON finance.gateway_settlement_import
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();
CREATE TRIGGER gateway_settlement_line_append_only BEFORE UPDATE OR DELETE ON finance.gateway_settlement_line
    FOR EACH ROW EXECUTE FUNCTION finance.forbid_history_change();

SELECT platform.apply_tenant_rls();
