-- Batch 6 foundation — contract change to Domain F (docs/batch6-contracts.md §6.3):
-- examination fees raised by Domain I through `ExamFeePort` (revaluation / rechecking fees) are
-- charges with `source_type = 'exam_fee'` and `source_id` = the Domain I aggregate id (one charge
-- per source: the port is idempotent on it).

DO $$
DECLARE
    c text;
BEGIN
    FOR c IN
        SELECT conname FROM pg_constraint
        WHERE conrelid = 'finance.charge'::regclass AND contype = 'c'
          AND pg_get_constraintdef(oid) LIKE '%source_type%'
    LOOP
        EXECUTE format('ALTER TABLE finance.charge DROP CONSTRAINT %I', c);
    END LOOP;
END $$;

ALTER TABLE finance.charge ADD CONSTRAINT charge_source_type_check CHECK (source_type IN (
    'fee_structure', 'manual', 'admission', 'debit_note', 'aid_recovery', 'fine', 'exam_fee'));

CREATE UNIQUE INDEX charge_exam_fee_source ON finance.charge (tenant_id, source_id)
    WHERE source_type = 'exam_fee';
