-- ERP_EXTENSIONS 1D — configurable late fees and fee reminders. Logic ported from
-- HeyPuter/everything invoicing latefee.js / reminders.js / aging.js (MIT); see THIRD_PARTY_NOTICES.md.

-- A new governed rule: when reminders go out (before the due date, and the dunning days).
ALTER TABLE finance.finance_rule DROP CONSTRAINT IF EXISTS finance_rule_rule_key_check;
ALTER TABLE finance.finance_rule ADD CONSTRAINT finance_rule_rule_key_check
    CHECK (rule_key IN ('late_fee', 'rounding', 'instalment_policy', 'refund_window',
                        'write_off_threshold', 'reminder_schedule'));

-- Late fees are charges (`source_type` 'fine', `source_id` = the late charge). One accrual per
-- charge per day keeps a run idempotent; the rule version that priced it is kept for audit.
CREATE TABLE finance.late_fee_accrual (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    charge_id            uuid           NOT NULL REFERENCES finance.charge (id),
    fee_charge_id        uuid           NOT NULL REFERENCES finance.charge (id),
    rule_id              uuid           REFERENCES finance.finance_rule (id),
    as_of                date           NOT NULL,
    days_late            integer        NOT NULL CHECK (days_late > 0),
    overdue_amount       numeric(18,2)  NOT NULL CHECK (overdue_amount > 0),
    fee_to_date          numeric(18,2)  NOT NULL CHECK (fee_to_date > 0),
    amount               numeric(18,2)  NOT NULL CHECK (amount > 0),
    created_by           text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, charge_id, as_of)
);
CREATE INDEX late_fee_accrual_institution_idx ON finance.late_fee_accrual (tenant_id, institution_id, as_of);
CREATE UNIQUE INDEX late_fee_accrual_fee_charge ON finance.late_fee_accrual (fee_charge_id);

-- Reminders before the due date (after it, 5B's dunning notices are the reminders).
CREATE TABLE finance.fee_reminder (
    id                   uuid           PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid           NOT NULL,
    institution_id       uuid           NOT NULL,
    charge_id            uuid           NOT NULL REFERENCES finance.charge (id),
    student_account_id   uuid           NOT NULL REFERENCES finance.student_account (id),
    due_date             date           NOT NULL,
    days_before          integer        NOT NULL CHECK (days_before > 0),
    amount_due           numeric(18,2)  NOT NULL CHECK (amount_due > 0),
    reminder_date        date           NOT NULL,
    message              text           NOT NULL,
    sent_by              text,
    created_at           timestamptz    NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, charge_id, due_date, days_before)
);
CREATE INDEX fee_reminder_institution_idx ON finance.fee_reminder (tenant_id, institution_id, reminder_date);

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.late_fee_accrual, finance.fee_reminder TO sutra_finance;

-- Forced row-level security on the new tenant tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
