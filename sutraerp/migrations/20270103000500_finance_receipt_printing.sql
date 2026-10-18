-- ERP_EXTENSIONS 3E — thermal receipt printing at the fee counter. The ESC/POS layout is ported
-- from HeyPuter/everything pos/src/lib/escpos.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- What an institution prints on its counter slips. The printer itself (Bluetooth, USB, paper
-- width) belongs to the counter's device and stays in its browser, never here.
CREATE TABLE finance.receipt_print_settings (
    tenant_id             uuid         NOT NULL,
    institution_id        uuid         NOT NULL,
    -- Empty means the institution's own name.
    title                 text         NOT NULL DEFAULT '' CHECK (length(title) <= 120),
    address               text         NOT NULL DEFAULT '' CHECK (length(address) <= 300),
    -- At most two { label, number } lines (GSTIN, PAN, trust registration).
    registrations         jsonb        NOT NULL DEFAULT '[]'::jsonb
                                       CHECK (jsonb_typeof(registrations) = 'array'
                                              AND jsonb_array_length(registrations) <= 2),
    footer                text         NOT NULL DEFAULT '' CHECK (length(footer) <= 300),
    show_amount_in_words  boolean      NOT NULL DEFAULT true,
    updated_by            text,
    updated_at            timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

-- The counter day close reads a day's receipts by institution and issue time.
CREATE INDEX IF NOT EXISTS receipt_institution_issued_idx
    ON finance.receipt (tenant_id, institution_id, issued_at);

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.receipt_print_settings TO sutra_finance;

-- Forced row-level security on the new tenant table (Part 1 S3).
SELECT platform.apply_tenant_rls();
