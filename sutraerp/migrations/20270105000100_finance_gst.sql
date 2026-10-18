-- ERP_EXTENSIONS 5D — GST tax invoices, bills of supply and GST credit notes
-- (docs/extensions-5d-gst.md). The document arithmetic (named taxes, tax-inclusive prices,
-- per-row rounding, zero-tax notes, document numbers) is ported from HeyPuter/everything
-- invoicing/src/lib/{money,taxnote,doc}.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- Charges stay the obligation: a charge raised on a taxable fee head keeps its lines at the
-- taxable value and gets one extra line per tax row (system fee heads GST-CGST / GST-SGST /
-- GST-IGST whose income accounts are the output-tax liability accounts 2510 / 2520 / 2530), so
-- publication posts Dr receivables · Cr income · Cr output tax with no new posting path. The
-- tables below are the GST snapshot of that charge, the document it was issued as, and the
-- per-financial-year series those documents are numbered from.

-- ------------------------------------------------------------------ institution GST profile
CREATE TABLE finance.gst_profile (
    tenant_id            uuid         NOT NULL,
    institution_id       uuid         NOT NULL,
    registration_type    text         NOT NULL DEFAULT 'regular'
                                      CHECK (registration_type IN ('regular', 'composition', 'unregistered')),
    -- 15 characters with its check character (validated by sutra_kernel::india::gstin_valid);
    -- absent only for an unregistered institution.
    gstin                text         CHECK (gstin ~ '^[0-9]{2}[0-9A-Z]{13}$'),
    legal_name           text         NOT NULL CHECK (length(btrim(legal_name)) BETWEEN 1 AND 200),
    trade_name           text         NOT NULL DEFAULT '' CHECK (length(trade_name) <= 200),
    address              text         NOT NULL DEFAULT '' CHECK (length(address) <= 300),
    state_code           text         NOT NULL CHECK (state_code ~ '^[0-9]{2}$'),
    -- '' nothing, 'preset:…' a shipped sentence, anything else printed verbatim.
    default_tax_note     text         NOT NULL DEFAULT 'preset:educationExempt' CHECK (length(default_tax_note) <= 400),
    -- Series prefixes; `{FY}` resolves to the financial year (`26-27`). Numbers ≤ 16 characters.
    invoice_prefix       text         NOT NULL DEFAULT 'TI/{FY}/' CHECK (length(invoice_prefix) <= 16),
    bill_prefix          text         NOT NULL DEFAULT 'BOS/{FY}/' CHECK (length(bill_prefix) <= 16),
    credit_note_prefix   text         NOT NULL DEFAULT 'CN/{FY}/' CHECK (length(credit_note_prefix) <= 16),
    enabled              boolean      NOT NULL DEFAULT true,
    version              bigint       NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id),
    CHECK (registration_type = 'unregistered' OR gstin IS NOT NULL)
);

-- ------------------------------------------------------------------ fee head tax setup
CREATE TABLE finance.fee_head_tax (
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    fee_head_id          uuid          NOT NULL REFERENCES finance.fee_head (id),
    -- SAC (services, 99xxxx) or HSN (goods: uniforms, books), 4 to 8 digits.
    sac                  text          NOT NULL CHECK (sac ~ '^[0-9]{4,8}$'),
    gst_rate             numeric(5,2)  NOT NULL DEFAULT 0 CHECK (gst_rate IN (0, 5, 12, 18, 28, 40)),
    exempt               boolean       NOT NULL DEFAULT false,
    -- The fee head's own zero-tax note ('' inherits the profile's, 'none' prints nothing).
    tax_note             text          NOT NULL DEFAULT '' CHECK (length(tax_note) <= 400),
    -- true: the fee head's amounts already include GST (the tax is carved out).
    tax_inclusive        boolean       NOT NULL DEFAULT false,
    version              bigint        NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, fee_head_id),
    CHECK (NOT exempt OR gst_rate = 0)
);
CREATE INDEX fee_head_tax_institution_idx ON finance.fee_head_tax (tenant_id, institution_id);

-- ------------------------------------------------------------------ recipient (payer) GST details
-- Place of supply and, for a registered payer (a sponsoring company), its GSTIN (B2B).
CREATE TABLE finance.gst_party (
    tenant_id            uuid         NOT NULL,
    student_account_id   uuid         NOT NULL REFERENCES finance.student_account (id),
    state_code           text         NOT NULL CHECK (state_code ~ '^[0-9]{2}$'),
    gstin                text         CHECK (gstin ~ '^[0-9]{2}[0-9A-Z]{13}$'),
    legal_name           text         NOT NULL DEFAULT '' CHECK (length(legal_name) <= 200),
    version              bigint       NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, student_account_id)
);

-- ------------------------------------------------------------------ document series per FY
CREATE TABLE finance.gst_series (
    tenant_id            uuid     NOT NULL,
    institution_id       uuid     NOT NULL,
    kind                 text     NOT NULL CHECK (kind IN ('tax_invoice', 'bill_of_supply', 'credit_note')),
    fy_start             integer  NOT NULL CHECK (fy_start BETWEEN 2000 AND 2999),
    next_value           bigint   NOT NULL DEFAULT 1 CHECK (next_value > 0),
    PRIMARY KEY (tenant_id, institution_id, kind, fy_start)
);

-- ------------------------------------------------------------------ GST documents
CREATE TABLE finance.gst_document (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    kind                 text          NOT NULL CHECK (kind IN ('tax_invoice', 'bill_of_supply', 'credit_note')),
    -- pending: the charge is not published yet (no number); issued; cancelled (credit note reversed).
    status               text          NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'issued', 'cancelled')),
    charge_id            uuid          NOT NULL REFERENCES finance.charge (id),
    adjustment_id        uuid          REFERENCES finance.adjustment (id),
    original_document_id uuid          REFERENCES finance.gst_document (id),
    document_number      text          CHECK (length(document_number) BETWEEN 1 AND 16
                                              AND document_number ~ '^[A-Za-z0-9/-]+$'),
    fy_start             integer,
    document_date        date,
    supplier_gstin       text,
    supplier_state       text          NOT NULL CHECK (supplier_state ~ '^[0-9]{2}$'),
    place_of_supply      text          NOT NULL CHECK (place_of_supply ~ '^[0-9]{2}$'),
    inter_state          boolean       NOT NULL,
    recipient_name       text          NOT NULL,
    recipient_gstin      text,
    taxable_value        numeric(18,2) NOT NULL,
    cgst                 numeric(18,2) NOT NULL DEFAULT 0,
    sgst                 numeric(18,2) NOT NULL DEFAULT 0,
    igst                 numeric(18,2) NOT NULL DEFAULT 0,
    total                numeric(18,2) NOT NULL,
    tax_note             text          NOT NULL DEFAULT '',
    created_at           timestamptz   NOT NULL DEFAULT now(),
    issued_at            timestamptz,
    cancelled_at         timestamptz,
    CHECK ((status = 'pending') = (document_number IS NULL)),
    CHECK ((kind = 'credit_note') = (adjustment_id IS NOT NULL)),
    CHECK (total = taxable_value + cgst + sgst + igst)
);
-- Numbers are unique per institution and financial year across every series.
CREATE UNIQUE INDEX gst_document_number_per_fy
    ON finance.gst_document (tenant_id, institution_id, fy_start, document_number)
    WHERE document_number IS NOT NULL;
-- One invoice / bill of supply per charge, one credit note per adjustment.
CREATE UNIQUE INDEX gst_document_one_per_charge
    ON finance.gst_document (tenant_id, charge_id) WHERE kind <> 'credit_note';
CREATE UNIQUE INDEX gst_document_one_per_adjustment
    ON finance.gst_document (tenant_id, adjustment_id) WHERE adjustment_id IS NOT NULL;
CREATE INDEX gst_document_period_idx
    ON finance.gst_document (tenant_id, institution_id, document_date);

-- Supplies on a document (the charge's own lines, never its tax lines).
CREATE TABLE finance.gst_document_line (
    tenant_id            uuid          NOT NULL,
    gst_document_id      uuid          NOT NULL REFERENCES finance.gst_document (id),
    line_no              integer       NOT NULL CHECK (line_no > 0),
    charge_line_no       integer,
    description          text          NOT NULL,
    sac                  text,
    treatment            text          NOT NULL CHECK (treatment IN ('taxable', 'nil', 'exempt', 'non_gst')),
    gst_rate             numeric(5,2)  NOT NULL DEFAULT 0,
    taxable_value        numeric(18,2) NOT NULL,
    cgst                 numeric(18,2) NOT NULL DEFAULT 0,
    sgst                 numeric(18,2) NOT NULL DEFAULT 0,
    igst                 numeric(18,2) NOT NULL DEFAULT 0,
    PRIMARY KEY (gst_document_id, line_no)
);

-- Tax rows of a document (`CGST @ 9%`), with the charge line that carries each on a charge.
CREATE TABLE finance.gst_document_tax (
    tenant_id            uuid          NOT NULL,
    gst_document_id      uuid          NOT NULL REFERENCES finance.gst_document (id),
    component            text          NOT NULL CHECK (component IN ('cgst', 'sgst', 'igst')),
    rate                 numeric(5,2)  NOT NULL CHECK (rate > 0),
    amount               numeric(18,2) NOT NULL CHECK (amount > 0),
    charge_line_no       integer,
    PRIMARY KEY (gst_document_id, component, rate)
);

GRANT SELECT, INSERT, UPDATE, DELETE ON finance.gst_profile, finance.fee_head_tax, finance.gst_party,
    finance.gst_series, finance.gst_document, finance.gst_document_line, finance.gst_document_tax
    TO sutra_finance;

-- Forced row-level security on the new tenant tables (Part 1 S3).
SELECT platform.apply_tenant_rls();
