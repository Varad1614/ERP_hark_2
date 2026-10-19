-- Batch 8 foundation — contract change to Domain F (docs/batch8-contracts.md §6.2):
-- campus service charges raised by Domain G through `CampusChargePort` (library fines, hostel fees /
-- damage, transport pass fees) are charges with `source_type = 'campus_charge'` and `source_id` =
-- the Domain G aggregate id (one charge per source: the port is idempotent on it). Procurement
-- postings (Domain J, `ProcurementPostingPort`) need no schema change: journal `source_type` is free
-- text (`procurement_invoice`, `procurement_payment`) and the AP accounts are seeded on first use.

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
    'fee_structure', 'manual', 'admission', 'debit_note', 'aid_recovery', 'fine', 'exam_fee',
    'campus_charge'));

CREATE UNIQUE INDEX charge_campus_charge_source ON finance.charge (tenant_id, source_id)
    WHERE source_type = 'campus_charge';
