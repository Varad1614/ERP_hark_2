-- Part 1 Stage 2 (DEF-E-007): payroll disbursement — net pay is paid out and the payroll
-- liabilities are cleared.
--
-- * people.staff_bank_account — the salary bank account of a staff member (beneficiary of the
--   NEFT bulk transfer file). One live row per staff member.
-- * people.payroll_disbursement — one batch per payroll run and payout kind (`net_pay` = bank
--   transfer of net pay to staff, `statutory_dues` = remittance of deductions / statutory dues by
--   challan). Lifecycle: pending_approval → approved → paid (or rejected; a rejected batch may be
--   recreated). Marking a batch paid posts Dr 2410 / Cr bank (net pay) or Dr 2420 / Cr bank
--   (statutory dues) in Finance through `PayrollPostingPort::post_payroll_payout`.
-- * people.payroll_disbursement_line — the payees (net pay: one line per staff member with a
--   snapshot of the bank details at batch creation) or the dues (statutory: one line per
--   deduction / employer-contribution component code).
--
-- There is no bank API: the transfer file is generated for manual upload to the bank portal and
-- `bank_submission` stays `blocked_external` (BLOCKED_EXTERNAL) until a bank integration exists.

CREATE TABLE people.staff_bank_account (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    staff_id             uuid         NOT NULL REFERENCES people.staff_member (id),
    account_holder_name  text         NOT NULL CHECK (length(btrim(account_holder_name)) BETWEEN 1 AND 140),
    account_number       text         NOT NULL CHECK (account_number ~ '^[0-9]{6,18}$'),
    ifsc                 text         NOT NULL CHECK (ifsc ~ '^[A-Z]{4}0[A-Z0-9]{6}$'),
    bank_name            text         CHECK (bank_name IS NULL OR length(bank_name) <= 140),
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, staff_id)
);

CREATE TABLE people.payroll_disbursement (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    run_id               uuid         NOT NULL REFERENCES people.payroll_run (id),
    number               text         NOT NULL CHECK (number ~ '^PD-[0-9]{6,}$'),
    kind                 text         NOT NULL CHECK (kind IN ('net_pay', 'statutory_dues')),
    status               text         NOT NULL
        CHECK (status IN ('pending_approval', 'approved', 'rejected', 'paid')),
    status_reason        text,
    line_count           integer      NOT NULL CHECK (line_count >= 1),
    total_amount         numeric(14,2) NOT NULL CHECK (total_amount > 0),
    -- No bank API: the file is uploaded by hand; submission stays BLOCKED_EXTERNAL.
    bank_submission      text         NOT NULL DEFAULT 'blocked_external'
        CHECK (bank_submission IN ('blocked_external')),
    file_generated_at    timestamptz,
    payment_reference    text         CHECK (payment_reference IS NULL OR length(payment_reference) BETWEEN 1 AND 64),
    paid_on              date,
    paid_by              text,
    paid_at              timestamptz,
    journal_entry_id     uuid,
    journal_entry_number text,
    process_instance_id  text,
    approved_at          timestamptz,
    created_by           text,
    version              bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    CHECK (status <> 'paid' OR (payment_reference IS NOT NULL AND paid_on IS NOT NULL
                                AND journal_entry_id IS NOT NULL))
);
-- Idempotent: one live batch per run and kind (a rejected batch can be recreated).
CREATE UNIQUE INDEX payroll_disbursement_one_live
    ON people.payroll_disbursement (tenant_id, run_id, kind) WHERE status <> 'rejected';
CREATE UNIQUE INDEX payroll_disbursement_number
    ON people.payroll_disbursement (tenant_id, institution_id, number);
CREATE INDEX payroll_disbursement_institution
    ON people.payroll_disbursement (tenant_id, institution_id, created_at DESC);

CREATE TABLE people.payroll_disbursement_line (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    disbursement_id      uuid         NOT NULL REFERENCES people.payroll_disbursement (id) ON DELETE CASCADE,
    line_no              integer      NOT NULL CHECK (line_no >= 1),
    -- net pay: the payee
    staff_id             uuid         REFERENCES people.staff_member (id),
    result_id            uuid         REFERENCES people.payroll_result (id),
    staff_number         text,
    display_name         text,
    account_holder_name  text,
    account_number       text,
    ifsc                 text,
    -- statutory dues: the component remitted
    component_code       text,
    component_name       text,
    amount               numeric(14,2) NOT NULL CHECK (amount > 0),
    UNIQUE (disbursement_id, line_no),
    CHECK ((staff_id IS NOT NULL AND account_number IS NOT NULL AND ifsc IS NOT NULL)
           OR component_code IS NOT NULL)
);
