-- P4 integration (ERP_EXTENSIONS ⨝ erp_merge): finance.charge.source_type is the union of both
-- branches. erp_merge's 20261231044001_platform_data_import added `opening_balance`; ERP_EXTENSIONS'
-- 20270101000500_finance_billing_runs (which runs later) re-created the check from the older list
-- plus `billing_run`, dropping `opening_balance`. Re-create it with every source.
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
    'campus_charge', 'opening_balance', 'billing_run'));
