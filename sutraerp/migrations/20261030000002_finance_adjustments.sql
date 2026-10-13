-- Batch 5 slice 5A: financial adjustments (credit / debit notes, write-offs, waivers,
-- concessions) with WorkflowPort approval, posted reversal and corrected successor records
-- (L3-F-020 … F-024). The charge's `adjustment_amount` moves only through
-- `shared::charge::apply_adjustment`; the ledger only through `shared::posting`.
CREATE TABLE finance.adjustment (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    adjustment_number    text          NOT NULL,
    charge_id            uuid          NOT NULL REFERENCES finance.charge (id),
    student_account_id   uuid          NOT NULL REFERENCES finance.student_account (id),
    kind                 text          NOT NULL CHECK (kind IN ('credit_note', 'debit_note',
                                           'write_off', 'waiver', 'concession')),
    -- Always positive; the direction follows the kind (debit note +, everything else −).
    amount               numeric(18,2) NOT NULL CHECK (amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    reason               text          NOT NULL CHECK (length(btrim(reason)) > 0),
    -- Where it came from: `manual`, `charge_revision`, `dispute`, `correction`.
    origin               text          NOT NULL DEFAULT 'manual' CHECK (origin IN ('manual',
                                           'charge_revision', 'dispute', 'correction')),
    status               text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft',
                                           'pending_approval', 'approved', 'posted', 'rejected',
                                           'reversed', 'superseded')),
    status_reason        text,
    process_instance_id  text,
    decided_by           text,
    decided_at           timestamptz,
    posted_at            timestamptz,
    posting_date         date,
    journal_entry_id     uuid          REFERENCES finance.journal_entry (id),
    reversal_entry_id    uuid          REFERENCES finance.journal_entry (id),
    reversed_at          timestamptz,
    supersedes_id        uuid          REFERENCES finance.adjustment (id),
    superseded_by_id     uuid          REFERENCES finance.adjustment (id),
    created_by           text,
    version              bigint        NOT NULL DEFAULT 1,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX adjustment_number_per_institution
    ON finance.adjustment (tenant_id, institution_id, adjustment_number);
CREATE INDEX adjustment_charge_idx ON finance.adjustment (tenant_id, charge_id);
CREATE INDEX adjustment_institution_idx ON finance.adjustment (tenant_id, institution_id, status);
-- A record is superseded by at most one successor.
CREATE UNIQUE INDEX adjustment_one_successor
    ON finance.adjustment (tenant_id, supersedes_id) WHERE supersedes_id IS NOT NULL;
