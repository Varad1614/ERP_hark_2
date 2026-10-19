-- ERP_EXTENSIONS 3D — staff expense claims. Logic ported from HeyPuter/everything expense
-- src/lib/ocr.js, money.js, reports.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- A staff member captures receipts (stored here, read by the PaddleOCR sidecar when one is
-- configured; the reading only pre-fills a draft), records expenses and bundles them into a
-- claim that moves draft → submitted → approved → reimbursed (withdraw / reject send a
-- submitted claim back to draft). Lines of a submitted claim are locked. Approval books the
-- claim as a payable to the staff member (Dr 5510 / Cr 2310 through the procurement posting
-- port); reimbursement books the payment (Dr 2310 / Cr bank).
CREATE TABLE resources.expense_settings (
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    -- A receipt is required for any expense above this amount (₹).
    receipt_required_above numeric(14,2) NOT NULL DEFAULT 250 CHECK (receipt_required_above >= 0),
    -- Expenses older than this many days cannot be claimed (0 = no limit).
    max_age_days         integer       NOT NULL DEFAULT 90 CHECK (max_age_days BETWEEN 0 AND 730),
    -- [{ id, name, limit, receiptAbove }]
    categories           jsonb         NOT NULL CHECK (jsonb_typeof(categories) = 'array'),
    version              bigint        NOT NULL DEFAULT 1,
    updated_by           text,
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

-- Receipt images and PDFs (at most 1.5 MB each; the screen downsizes photos before upload).
CREATE TABLE resources.expense_receipt (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    staff_id             uuid          NOT NULL,
    file_name            text          NOT NULL CHECK (length(file_name) BETWEEN 1 AND 200),
    content_type         text          NOT NULL CHECK (content_type IN ('image/jpeg', 'image/png',
                             'image/webp', 'application/pdf')),
    byte_size            integer       NOT NULL CHECK (byte_size BETWEEN 1 AND 1500000),
    sha256               bytea         NOT NULL CHECK (octet_length(sha256) = 32),
    data                 bytea         NOT NULL,
    -- read | failed | disabled
    ocr_status           text          NOT NULL CHECK (ocr_status IN ('read', 'failed', 'disabled')),
    ocr_engine           text          NOT NULL,
    ocr_error            text          CHECK (ocr_error IS NULL OR length(ocr_error) <= 500),
    ocr_lines            jsonb         NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(ocr_lines) = 'array'),
    ocr_fields           jsonb         CHECK (ocr_fields IS NULL OR jsonb_typeof(ocr_fields) = 'object'),
    created_by           text,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT expense_receipt_once UNIQUE (tenant_id, staff_id, sha256)
);
CREATE INDEX expense_receipt_staff_idx ON resources.expense_receipt (tenant_id, staff_id, created_at);

CREATE TABLE resources.expense_claim (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    staff_id             uuid          NOT NULL,
    number               text          NOT NULL,
    title                text          NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    purpose              text          CHECK (purpose IS NULL OR length(purpose) <= 2000),
    status               text          NOT NULL DEFAULT 'draft'
                             CHECK (status IN ('draft', 'submitted', 'approved', 'reimbursed')),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    -- Total of the lines, frozen at submission.
    total                numeric(14,2) NOT NULL DEFAULT 0 CHECK (total >= 0),
    submitted_at         timestamptz,
    approved_at          timestamptz,
    reimbursed_at        timestamptz,
    decided_by           text,
    decision_note        text          CHECK (decision_note IS NULL OR length(decision_note) <= 2000),
    -- Set when an approver sends the claim back; cleared on the next submission.
    returned_reason      text          CHECK (returned_reason IS NULL OR length(returned_reason) <= 2000),
    approval_entry       text,
    payment_entry        text,
    payment_reference    text          CHECK (payment_reference IS NULL OR length(payment_reference) <= 120),
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by           text,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT expense_claim_number_unique UNIQUE (tenant_id, institution_id, number)
);
CREATE INDEX expense_claim_staff_idx ON resources.expense_claim (tenant_id, staff_id, status);
CREATE INDEX expense_claim_institution_idx ON resources.expense_claim (tenant_id, institution_id, status);

CREATE TABLE resources.expense (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    staff_id             uuid          NOT NULL,
    claim_id             uuid          REFERENCES resources.expense_claim (id),
    spent_on             date          NOT NULL,
    merchant             text          NOT NULL CHECK (length(btrim(merchant)) BETWEEN 1 AND 120),
    amount               numeric(14,2) NOT NULL CHECK (amount > 0),
    currency             text          NOT NULL DEFAULT 'INR' CHECK (currency ~ '^[A-Z]{3}$'),
    category_id          text          CHECK (category_id IS NULL OR length(category_id) <= 60),
    payment_method       text          CHECK (payment_method IS NULL OR payment_method IN ('Card',
                             'Cash', 'Bank transfer', 'Mobile payment', 'Other')),
    tax                  numeric(14,2) CHECK (tax IS NULL OR tax >= 0),
    gstin                text          CHECK (gstin IS NULL OR gstin ~ '^[0-9]{2}[A-Z0-9]{13}$'),
    notes                text          CHECK (notes IS NULL OR length(notes) <= 2000),
    items                jsonb         NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(items) = 'array'),
    receipt_id           uuid          REFERENCES resources.expense_receipt (id),
    -- What OCR pre-filled when this expense was created from a receipt (kept for the approver).
    ocr                  jsonb         CHECK (ocr IS NULL OR jsonb_typeof(ocr) = 'object'),
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_by           text,
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT expense_tax_le_amount CHECK (tax IS NULL OR tax <= amount)
);
CREATE INDEX expense_staff_idx ON resources.expense (tenant_id, staff_id, spent_on);
CREATE INDEX expense_claim_idx ON resources.expense (tenant_id, claim_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON resources.expense_settings, resources.expense,
    resources.expense_claim TO sutra_resources;
GRANT SELECT, INSERT, DELETE ON resources.expense_receipt TO sutra_resources;
SELECT platform.apply_tenant_rls();
