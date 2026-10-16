-- Part 2 · Stage 4 (S4-import) — data migration: import jobs, mapping profiles, row ledger, plus
-- the two owning-domain changes the importers need (docs/readiness/part2/s4-import.md).
--
-- * platform.import_mapping_profile — saved column mappings (source header → canonical column)
--   per tenant and entity, reused for the next file of the same legacy export.
-- * platform.import_job — one uploaded file: format, SHA-256, idempotent job key (unique per
--   tenant), mapping, on-existing policy, status, dry-run / commit counters, reconciliation
--   totals, approval (separation of duties for opening fee balances) and who did what.
-- * platform.import_job_row — every parsed row (raw cells by source header), its natural key and
--   its dry-run / commit outcome (action, errors, record id, amount). The downloadable error
--   report is built from it.
-- * finance.charge source `opening_balance` — a migrated student balance is an ordinary,
--   payable charge published through Finance's posting code (Dr 1300 receivables / Cr 3100
--   accumulated fund); one per student account (unique index), so re-imports never duplicate.
-- * student.historical_result — past course results migrated from the legacy system
--   (append-only), with passed credits posted to the Domain C credit ledger.
--
-- Every new table carries tenant_id (and institution_id) and gets forced row-level security from
-- platform.apply_tenant_rls() at the end.

CREATE TABLE platform.import_mapping_profile (
    id          uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id   uuid        NOT NULL,
    entity      text        NOT NULL CHECK (entity ~ '^[a-z_]{2,40}$'),
    name        text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
    -- {"<source header>": "<canonical column>"}
    mapping     jsonb       NOT NULL CHECK (jsonb_typeof(mapping) = 'object'),
    created_by  text,
    version     bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT import_mapping_profile_name_unique UNIQUE (tenant_id, entity, name)
);

CREATE TABLE platform.import_job (
    id                      uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid          NOT NULL,
    institution_id          uuid          NOT NULL,
    entity                  text          NOT NULL CHECK (entity ~ '^[a-z_]{2,40}$'),
    -- Idempotency of the upload: the same key returns the same job (default
    -- `<entity>:<institution>:<file sha256>`).
    job_key                 text          NOT NULL CHECK (length(btrim(job_key)) BETWEEN 1 AND 200),
    file_name               text          NOT NULL CHECK (length(btrim(file_name)) BETWEEN 1 AND 255),
    file_format             text          NOT NULL CHECK (file_format IN ('csv', 'xlsx')),
    file_sha256             text          NOT NULL CHECK (file_sha256 ~ '^[0-9a-f]{64}$'),
    file_bytes              integer       NOT NULL CHECK (file_bytes >= 0),
    headers                 jsonb         NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(headers) = 'array'),
    mapping                 jsonb         NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(mapping) = 'object'),
    mapping_profile_id      uuid          REFERENCES platform.import_mapping_profile (id) ON DELETE SET NULL,
    on_existing             text          NOT NULL DEFAULT 'skip' CHECK (on_existing IN ('skip', 'update')),
    batch_size              integer       NOT NULL DEFAULT 500 CHECK (batch_size BETWEEN 1 AND 5000),
    status                  text          NOT NULL DEFAULT 'uploaded' CHECK (status IN (
                                'uploaded', 'validated', 'invalid', 'pending_approval',
                                'committing', 'committed', 'failed', 'rejected')),
    row_count               integer       NOT NULL DEFAULT 0 CHECK (row_count >= 0),
    valid_count             integer       NOT NULL DEFAULT 0 CHECK (valid_count >= 0),
    error_count             integer       NOT NULL DEFAULT 0 CHECK (error_count >= 0),
    duplicate_in_file_count integer       NOT NULL DEFAULT 0 CHECK (duplicate_in_file_count >= 0),
    existing_count          integer       NOT NULL DEFAULT 0 CHECK (existing_count >= 0),
    created_count           integer       NOT NULL DEFAULT 0 CHECK (created_count >= 0),
    updated_count           integer       NOT NULL DEFAULT 0 CHECK (updated_count >= 0),
    skipped_count           integer       NOT NULL DEFAULT 0 CHECK (skipped_count >= 0),
    batches_committed       integer       NOT NULL DEFAULT 0 CHECK (batches_committed >= 0),
    -- Reconciliation (entities with money): Σ amount column of the file, Σ amounts the commit
    -- posted, Σ the ledger holds for the job's records.
    file_total              numeric(16,2),
    committed_total         numeric(16,2),
    ledger_total            numeric(16,2),
    reconciled              boolean,
    failure_reason          text,
    process_instance_id     text,
    created_by              text,
    validated_at            timestamptz,
    validated_ms            integer,
    commit_requested_by     text,
    commit_requested_at     timestamptz,
    decided_by              text,
    decided_at              timestamptz,
    decision_reason         text,
    committed_by            text,
    committed_at            timestamptz,
    committed_ms            integer,
    version                 bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at              timestamptz   NOT NULL DEFAULT now(),
    updated_at              timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT import_job_key_unique UNIQUE (tenant_id, job_key)
);
CREATE INDEX import_job_institution_idx
    ON platform.import_job (tenant_id, institution_id, created_at DESC);

CREATE TABLE platform.import_job_row (
    job_id          uuid          NOT NULL REFERENCES platform.import_job (id),
    tenant_id       uuid          NOT NULL,
    institution_id  uuid          NOT NULL,
    -- 1-based source line (header = line 1).
    line            integer       NOT NULL CHECK (line >= 2),
    -- Cells keyed by source header, so the mapping can change after the upload.
    raw             jsonb         NOT NULL CHECK (jsonb_typeof(raw) = 'object'),
    natural_key     text,
    action          text          CHECK (action IN ('create', 'update', 'skip', 'error')),
    errors          jsonb         NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(errors) = 'array'),
    record_id       uuid,
    amount          numeric(16,2),
    committed       boolean       NOT NULL DEFAULT false,
    PRIMARY KEY (job_id, line)
);
CREATE INDEX import_job_row_key_idx ON platform.import_job_row (job_id, natural_key);

-- No runtime role deletes import evidence (jobs are kept as the migration's audit trail).
REVOKE DELETE, TRUNCATE ON platform.import_job, platform.import_job_row FROM sutra_platform;

-- ------------------------------------------------------------------ finance: opening balances
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
    'campus_charge', 'opening_balance'));

-- One opening balance per student finance account (source_id = finance.student_account.id).
CREATE UNIQUE INDEX charge_opening_balance_source ON finance.charge (tenant_id, source_id)
    WHERE source_type = 'opening_balance';

-- ------------------------------------------------------------------ student: historical results
CREATE TABLE student.historical_result (
    id                      uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid          NOT NULL,
    institution_id          uuid          NOT NULL,
    student_id              uuid          NOT NULL REFERENCES student.student (id),
    student_context_id      uuid          NOT NULL REFERENCES student.student_context (id),
    -- Legacy labels (the legacy term / year need not exist in the Domain B calendar).
    term_label              text          NOT NULL CHECK (length(btrim(term_label)) BETWEEN 1 AND 40),
    academic_year_label     text          CHECK (academic_year_label IS NULL OR length(btrim(academic_year_label)) BETWEEN 1 AND 20),
    course_code             text          NOT NULL CHECK (course_code ~ '^[A-Za-z0-9][A-Za-z0-9_./-]{0,31}$'),
    course_title            text          NOT NULL CHECK (length(btrim(course_title)) BETWEEN 1 AND 200),
    credits                 numeric(5,1)  NOT NULL CHECK (credits >= 0),
    grade                   text          CHECK (grade IS NULL OR length(btrim(grade)) BETWEEN 1 AND 8),
    grade_point             numeric(4,2)  CHECK (grade_point IS NULL OR grade_point BETWEEN 0 AND 10),
    marks                   numeric(7,2)  CHECK (marks IS NULL OR marks >= 0),
    max_marks               numeric(7,2)  CHECK (max_marks IS NULL OR max_marks > 0),
    result_status           text          NOT NULL CHECK (result_status IN ('pass', 'fail', 'absent', 'withheld', 'exempted')),
    attempt                 integer       NOT NULL DEFAULT 1 CHECK (attempt BETWEEN 1 AND 20),
    source                  text          NOT NULL DEFAULT 'data_migration',
    import_job_id           uuid,
    -- The `earned` credit-ledger entry of a passed result with credits.
    credit_ledger_entry_id  uuid          REFERENCES student.credit_ledger_entry (id),
    recorded_by             text,
    recorded_at             timestamptz   NOT NULL DEFAULT now(),
    CONSTRAINT historical_result_marks_range CHECK (marks IS NULL OR max_marks IS NULL OR marks <= max_marks),
    CONSTRAINT historical_result_unique UNIQUE (tenant_id, student_id, course_code, term_label, attempt)
);
CREATE INDEX historical_result_student_idx ON student.historical_result (tenant_id, student_id);
CREATE TRIGGER historical_result_immutable
    BEFORE UPDATE OR DELETE ON student.historical_result
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

SELECT platform.apply_tenant_rls();
