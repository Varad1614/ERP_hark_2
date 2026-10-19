-- Batch 3 slice 3B: academic progression (L2-C04 + amendment 2026-08-22, L3-C-027 … C-038, C-075).
--
-- Progression policies (versioned), progression decisions (+ conditions, probation and detention
-- cases, append-only revisions), the append-only Academic Credit Ledger, credit transfer /
-- recognition, programme migration, academic break + rejoin, the academic-standing projection,
-- qualification eligibility determinations (append-only; read by 3C) and ABC synchronisation
-- records. Domain B ids are plain uuids (ADR-L5-0036 §3.2).

CREATE TABLE student.progression_policy (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    -- NULL = institution-wide; otherwise the policy governs one programme.
    programme_id        uuid,
    code                text        NOT NULL CHECK (code ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,39}$'),
    version_no          integer     NOT NULL CHECK (version_no >= 1),
    name                text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 160),
    effective_from      date        NOT NULL,
    -- Evaluation thresholds (camelCase JSON, see `progression::policy::PolicyRules`).
    rules               jsonb       NOT NULL,
    status              text        NOT NULL DEFAULT 'draft'
        CONSTRAINT progression_policy_status_check CHECK (status IN ('draft', 'in_review', 'effective', 'superseded', 'retired')),
    status_reason       text,
    supersedes_policy_id uuid       REFERENCES student.progression_policy (id),
    process_instance_id text,
    effective_at        timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT progression_policy_version_unique UNIQUE (tenant_id, institution_id, code, version_no)
);
-- One effective version per policy code.
CREATE UNIQUE INDEX progression_policy_one_effective
    ON student.progression_policy (tenant_id, institution_id, code) WHERE status = 'effective';
-- One open (draft | in_review) version per code.
CREATE UNIQUE INDEX progression_policy_one_open
    ON student.progression_policy (tenant_id, institution_id, code) WHERE status IN ('draft', 'in_review');

CREATE TABLE student.progression_decision (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    academic_term_id       uuid        NOT NULL,
    -- Programme term the decision evaluates (context.current_term_no at evaluation).
    term_no                integer     NOT NULL CHECK (term_no >= 1),
    policy_id              uuid        REFERENCES student.progression_policy (id),
    outcome                text        NOT NULL CHECK (outcome IN (
        'progressed', 'conditionally_progressed', 'probation', 'detained', 'repeat', 'qualification_eligible')),
    -- Outcome the effective policy recommends for the evidence (NULL without policy).
    recommended_outcome    text,
    -- `results` (ResultsPort) or `manual` (entered by progression.manage while Exams is stubbed).
    evidence_source        text        NOT NULL CHECK (evidence_source IN ('results', 'manual')),
    credits_attempted      numeric(7,2),
    credits_earned         numeric(7,2) NOT NULL CHECK (credits_earned >= 0),
    failed_component_ids   uuid[]      NOT NULL DEFAULT '{}',
    sgpa                   numeric(5,2),
    cgpa                   numeric(5,2),
    evidence_note          text,
    -- Detention / repeat consequence (L3-C-031).
    repeat_unit            text        CHECK (repeat_unit IN ('term', 'year')),
    probation_review_due_on date,
    status                 text        NOT NULL DEFAULT 'draft'
        CONSTRAINT progression_decision_status_check CHECK (status IN ('draft', 'pending_approval', 'effective', 'superseded', 'rejected')),
    status_reason          text,
    supersedes_decision_id uuid        REFERENCES student.progression_decision (id),
    -- Context term after applying the decision (set when effective).
    resulting_term_no      integer,
    process_instance_id    text,
    decided_by             text,
    effective_at           timestamptz,
    version                bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX progression_decision_one_effective
    ON student.progression_decision (tenant_id, student_context_id, academic_term_id) WHERE status = 'effective';
CREATE UNIQUE INDEX progression_decision_one_open
    ON student.progression_decision (tenant_id, student_context_id, academic_term_id) WHERE status IN ('draft', 'pending_approval');
CREATE INDEX progression_decision_student_idx ON student.progression_decision (tenant_id, student_id, created_at DESC);

-- Append-only record of every fresh evaluation after re-evaluation / supplementary / appeal (L3-C-038).
CREATE TABLE student.progression_revision (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    -- The decision that was re-evaluated.
    original_decision_id   uuid        NOT NULL REFERENCES student.progression_decision (id),
    -- The subsequent decision (NULL when the fresh evaluation reached the same outcome).
    revised_decision_id    uuid        REFERENCES student.progression_decision (id),
    trigger                text        NOT NULL CHECK (trigger IN ('revaluation', 'supplementary', 'appeal', 'correction')),
    same_outcome           boolean     NOT NULL,
    evaluated_outcome      text        NOT NULL,
    reason                 text        NOT NULL,
    evidence_reference     text,
    actor                  text,
    created_at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX progression_revision_decision_idx ON student.progression_revision (tenant_id, original_decision_id);
CREATE TRIGGER progression_revision_immutable
    BEFORE UPDATE OR DELETE ON student.progression_revision
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.progression_condition (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    institution_id          uuid        NOT NULL,
    student_id              uuid        NOT NULL REFERENCES student.student (id),
    student_context_id      uuid        NOT NULL REFERENCES student.student_context (id),
    decision_id             uuid        NOT NULL REFERENCES student.progression_decision (id),
    description             text        NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 500),
    curriculum_component_id uuid,
    due_on                  date,
    status                  text        NOT NULL DEFAULT 'pending'
        CONSTRAINT progression_condition_status_check CHECK (status IN ('pending', 'fulfilled', 'failed', 'waived')),
    resolution_note         text,
    resolved_at             timestamptz,
    resolved_by             text,
    version                 bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX progression_condition_decision_idx ON student.progression_condition (decision_id);
CREATE INDEX progression_condition_student_idx ON student.progression_condition (tenant_id, student_id, status);

-- Append-only Academic Credit Ledger (L3-C-028). `credits` is the absolute amount; the kind
-- gives the sign (revoked / expired subtract, adjusted may be negative).
CREATE TABLE student.credit_ledger_entry (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    institution_id          uuid        NOT NULL,
    student_id              uuid        NOT NULL REFERENCES student.student (id),
    student_context_id      uuid        NOT NULL REFERENCES student.student_context (id),
    kind                    text        NOT NULL CHECK (kind IN ('earned', 'transferred_in', 'recognised', 'adjusted', 'revoked', 'expired')),
    credits                 numeric(7,2) NOT NULL,
    curriculum_component_id uuid,
    academic_term_id        uuid,
    -- What produced the entry: `manual`, `progression_decision`, `credit_transfer`, `programme_migration`.
    source_type             text        NOT NULL,
    source_id               uuid,
    reverses_entry_id       uuid        REFERENCES student.credit_ledger_entry (id),
    institution_approved    boolean     NOT NULL DEFAULT true,
    note                    text,
    recorded_by             text,
    recorded_at             timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT credit_ledger_sign CHECK (kind = 'adjusted' OR credits > 0),
    CONSTRAINT credit_ledger_adjusted_nonzero CHECK (kind <> 'adjusted' OR credits <> 0)
);
CREATE INDEX credit_ledger_student_idx ON student.credit_ledger_entry (tenant_id, student_id, recorded_at);
CREATE UNIQUE INDEX credit_ledger_one_reversal ON student.credit_ledger_entry (reverses_entry_id) WHERE reverses_entry_id IS NOT NULL;
CREATE TRIGGER credit_ledger_entry_immutable
    BEFORE UPDATE OR DELETE ON student.credit_ledger_entry
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.probation_case (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid        NOT NULL,
    student_id         uuid        NOT NULL REFERENCES student.student (id),
    student_context_id uuid        NOT NULL REFERENCES student.student_context (id),
    decision_id        uuid        NOT NULL REFERENCES student.progression_decision (id),
    review_due_on      date,
    status             text        NOT NULL DEFAULT 'active'
        CONSTRAINT probation_case_status_check CHECK (status IN ('active', 'cleared', 'escalated')),
    resolution_note    text,
    resolved_at        timestamptz,
    resolved_by        text,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX probation_case_one_active
    ON student.probation_case (tenant_id, student_context_id) WHERE status = 'active';

CREATE TABLE student.detention_case (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid        NOT NULL,
    student_id         uuid        NOT NULL REFERENCES student.student (id),
    student_context_id uuid        NOT NULL REFERENCES student.student_context (id),
    decision_id        uuid        NOT NULL REFERENCES student.progression_decision (id),
    -- Repeat consequence; NULL while undetermined (L4-C-031 continuation rule).
    repeat_unit        text        CHECK (repeat_unit IN ('term', 'year')),
    repeat_term_no     integer,
    determined_at      timestamptz,
    -- Set when a later effective decision (or revision) lifts the detention.
    released_at        timestamptz,
    release_note       text,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX detention_case_student_idx ON student.detention_case (tenant_id, student_id);

CREATE TABLE student.credit_transfer (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    institution_id        uuid        NOT NULL,
    student_id            uuid        NOT NULL REFERENCES student.student (id),
    student_context_id    uuid        NOT NULL REFERENCES student.student_context (id),
    -- `transferred_in` (credits earned elsewhere) or `recognised` (prior learning / MOOCs, …).
    kind                  text        NOT NULL CHECK (kind IN ('transferred_in', 'recognised')),
    source_institution    text        NOT NULL CHECK (length(btrim(source_institution)) BETWEEN 1 AND 200),
    source_programme      text,
    external_reference    text,
    evidence_note         text,
    -- [{title, credits, curriculumComponentId?}]
    items                 jsonb       NOT NULL,
    credits_requested     numeric(7,2) NOT NULL CHECK (credits_requested > 0),
    credits_recognised    numeric(7,2),
    status                text        NOT NULL DEFAULT 'requested'
        CONSTRAINT credit_transfer_status_check CHECK (status IN ('requested', 'under_review', 'approved', 'rejected', 'completed', 'withdrawn')),
    status_reason         text,
    revision_requested    boolean     NOT NULL DEFAULT false,
    process_instance_id   text,
    decided_by            text,
    version               bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX credit_transfer_student_idx ON student.credit_transfer (tenant_id, student_id, created_at DESC);

CREATE TABLE student.programme_migration (
    id                         uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                  uuid        NOT NULL,
    institution_id             uuid        NOT NULL,
    student_id                 uuid        NOT NULL REFERENCES student.student (id),
    student_context_id         uuid        NOT NULL REFERENCES student.student_context (id),
    from_programme_id          uuid        NOT NULL,
    from_curriculum_version_id uuid,
    from_term_no               integer,
    to_programme_id            uuid        NOT NULL,
    to_curriculum_version_id   uuid        NOT NULL,
    to_term_no                 integer     NOT NULL CHECK (to_term_no >= 1),
    -- Mapping of the current academic position / recognised credits ([{fromComponentId?, toComponentId?, credits, note?}]).
    credit_mapping             jsonb       NOT NULL DEFAULT '[]',
    reason                     text        NOT NULL,
    effective_on               date,
    status                     text        NOT NULL DEFAULT 'requested'
        CONSTRAINT programme_migration_status_check CHECK (status IN ('requested', 'under_review', 'approved', 'rejected', 'completed', 'withdrawn')),
    status_reason              text,
    process_instance_id        text,
    decided_by                 text,
    version                    bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                 timestamptz NOT NULL DEFAULT now(),
    updated_at                 timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX programme_migration_one_open
    ON student.programme_migration (tenant_id, student_context_id) WHERE status IN ('requested', 'under_review', 'approved');

CREATE TABLE student.academic_break (
    id                        uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid        NOT NULL,
    institution_id            uuid        NOT NULL,
    student_id                uuid        NOT NULL REFERENCES student.student (id),
    student_context_id        uuid        NOT NULL REFERENCES student.student_context (id),
    reason                    text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    starts_on                 date        NOT NULL,
    expected_return_on        date        NOT NULL,
    -- Term the student was in when the break started (preserved position, L4-C-034-BH-004).
    term_no_at_start          integer,
    status                    text        NOT NULL DEFAULT 'requested'
        CONSTRAINT academic_break_status_check CHECK (status IN ('requested', 'approved', 'in_effect', 'rejoined', 'lapsed', 'rejected', 'withdrawn')),
    status_reason             text,
    process_instance_id       text,
    -- Rejoining placement (L3-C-035).
    rejoin_term_no            integer,
    rejoin_curriculum_version_id uuid,
    rejoin_mapping_note       text,
    rejoined_on               date,
    -- Standing before the break, restored on rejoining (L4-C-034-BH-004).
    standing_before           text,
    started_at                timestamptz,
    version                   bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz NOT NULL DEFAULT now(),
    updated_at                timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT academic_break_dates CHECK (starts_on <= expected_return_on)
);
CREATE UNIQUE INDEX academic_break_one_open
    ON student.academic_break (tenant_id, student_context_id) WHERE status IN ('requested', 'approved', 'in_effect');

-- Projection: current academic standing per StudentContext (written by progression only).
CREATE TABLE student.academic_standing (
    student_context_id uuid        PRIMARY KEY REFERENCES student.student_context (id),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid        NOT NULL,
    student_id         uuid        NOT NULL REFERENCES student.student (id),
    standing           text        NOT NULL CHECK (standing IN ('good_standing', 'conditional', 'probation', 'detained', 'academic_break')),
    -- What set it: `progression_decision`, `probation_case`, `progression_condition`, `academic_break`, `detention_case`.
    source_type        text        NOT NULL,
    source_id          uuid,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

-- Append-only qualification eligibility determinations (L3-C-036). The newest row per context
-- is the current determination; 3C reads it (plain uuid reference, no FK).
CREATE TABLE student.qualification_eligibility (
    id                     uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id              uuid        NOT NULL,
    institution_id         uuid        NOT NULL,
    student_id             uuid        NOT NULL REFERENCES student.student (id),
    student_context_id     uuid        NOT NULL REFERENCES student.student_context (id),
    programme_id           uuid,
    curriculum_version_id  uuid,
    eligible               boolean     NOT NULL,
    -- QualificationOutcome wire names the student qualifies for (MEME aware).
    outcomes               text[]      NOT NULL DEFAULT '{}',
    total_credits          numeric(8,2) NOT NULL,
    required_credits       numeric(8,2),
    -- Outstanding conditions while pending (["minimum credits 80 not met (40 recorded)", …]).
    outstanding            text[]      NOT NULL DEFAULT '{}',
    -- `ledger` (computed from the credit ledger) or `manual` (curriculum without minimum credits).
    basis                  text        NOT NULL CHECK (basis IN ('ledger', 'manual')),
    note                   text,
    evaluated_by           text,
    evaluated_at           timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX qualification_eligibility_ctx_idx
    ON student.qualification_eligibility (tenant_id, student_context_id, evaluated_at DESC);
CREATE INDEX qualification_eligibility_student_idx
    ON student.qualification_eligibility (tenant_id, student_id, evaluated_at DESC);
CREATE TRIGGER qualification_eligibility_immutable
    BEFORE UPDATE OR DELETE ON student.qualification_eligibility
    FOR EACH ROW EXECUTE FUNCTION student.forbid_history_change();

CREATE TABLE student.abc_sync (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid        NOT NULL,
    student_id         uuid        NOT NULL REFERENCES student.student (id),
    student_context_id uuid        NOT NULL REFERENCES student.student_context (id),
    entry_ids          uuid[]      NOT NULL,
    total_credits      numeric(8,2) NOT NULL,
    status             text        NOT NULL DEFAULT 'pending'
        CONSTRAINT abc_sync_status_check CHECK (status IN ('pending', 'submitted', 'acknowledged', 'failed')),
    -- `not_configured` (no-op adapter), `rejected`, …
    failure_reason     text,
    external_reference text,
    attempted_at       timestamptz NOT NULL DEFAULT now(),
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX abc_sync_student_idx ON student.abc_sync (tenant_id, student_id, created_at DESC);
