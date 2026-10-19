-- Slice 2B: academic structure, programme catalogue, curriculum framework, school academic model
-- (L2-B04 + curriculum-retirement amendment 2026-08-24; L3/L4-B-023 … B-026).
--
-- Every versioned definition uses `PublicationStatus` (draft, in_review, published, superseded,
-- retired). Published content is immutable: a change creates a successor draft
-- (`supersedes_id` / `supersedes_version_id`) and publishing it supersedes the predecessor in the
-- same transaction. Triggers below make the immutability hold even against direct SQL.

-- ---------------------------------------------------------------------------------------------
-- Academic structure: the institution's levels / degree types / streams / disciplines.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE institution.academic_structure (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL REFERENCES institution.institution(id),
    name                text        NOT NULL,
    -- higher_education (programmes), school (school academic model), or mixed.
    model               text        NOT NULL CHECK (model IN ('higher_education', 'school', 'mixed')),
    nep_aligned         boolean     NOT NULL DEFAULT true,
    version_no          integer     NOT NULL CHECK (version_no >= 1),
    supersedes_id       uuid        REFERENCES institution.academic_structure(id),
    effective_from      date,
    status              text        NOT NULL DEFAULT 'draft' CHECK (status IN (
                            'draft', 'in_review', 'published', 'superseded', 'retired')),
    process_instance_id text,
    status_reason       text,
    published_at        timestamptz,
    retired_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT academic_structure_version_unique UNIQUE (tenant_id, institution_id, version_no)
);
CREATE UNIQUE INDEX academic_structure_one_published ON institution.academic_structure (institution_id)
    WHERE status = 'published';
CREATE UNIQUE INDEX academic_structure_one_open ON institution.academic_structure (institution_id)
    WHERE status IN ('draft', 'in_review');

CREATE TABLE institution.academic_structure_node (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    academic_structure_id uuid        NOT NULL REFERENCES institution.academic_structure(id),
    parent_key            text,
    kind                  text        NOT NULL CHECK (kind IN ('level', 'degree_type', 'stream', 'discipline')),
    key                   text        NOT NULL CHECK (key ~ '^[a-z0-9][a-z0-9_.-]{0,39}$'),
    name                  text        NOT NULL,
    -- NHEQF level (4.5 … 8) under NEP 2020, when the node is a level / degree type.
    nheqf_level           numeric(3,1) CHECK (nheqf_level BETWEEN 4.5 AND 8),
    sort_order            integer     NOT NULL DEFAULT 0,
    CONSTRAINT academic_structure_node_key_unique UNIQUE (academic_structure_id, key)
);

-- ---------------------------------------------------------------------------------------------
-- Programme catalogue.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE institution.programme (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL REFERENCES institution.institution(id),
    org_unit_id         uuid,       -- 2A org unit (no cross-slice FK, contract §3.1)
    code                text        NOT NULL,
    name                text        NOT NULL,
    level               text        NOT NULL,       -- level key of the academic structure
    degree_type         text,                       -- degree_type key of the academic structure
    duration_terms      integer     NOT NULL CHECK (duration_terms BETWEEN 1 AND 24),
    campus_ids          uuid[]      NOT NULL DEFAULT '{}',
    version_no          integer     NOT NULL DEFAULT 1 CHECK (version_no >= 1),
    status              text        NOT NULL DEFAULT 'draft' CHECK (status IN (
                            'draft', 'in_review', 'published', 'superseded', 'retired')),
    process_instance_id text,
    status_reason       text,
    published_at        timestamptz,
    retired_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT programme_code_unique UNIQUE (tenant_id, institution_id, code)
);
CREATE INDEX programme_institution_status ON institution.programme (tenant_id, institution_id, status);

-- ---------------------------------------------------------------------------------------------
-- Curriculum framework: one curriculum per programme lineage, immutable versions.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE institution.curriculum (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid        NOT NULL REFERENCES institution.institution(id),
    programme_id    uuid        NOT NULL REFERENCES institution.programme(id),
    code            text        NOT NULL,
    title           text        NOT NULL,
    -- draft until the first version is published; retired = CurriculumRetired.
    status          text        NOT NULL DEFAULT 'draft' CHECK (status IN (
                        'draft', 'in_review', 'published', 'superseded', 'retired')),
    process_instance_id text,
    status_reason   text,
    published_at    timestamptz,
    retired_at      timestamptz,
    version         bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT curriculum_code_unique UNIQUE (tenant_id, institution_id, code)
);
CREATE INDEX curriculum_programme ON institution.curriculum (tenant_id, programme_id);

CREATE TABLE institution.curriculum_version (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    institution_id          uuid        NOT NULL REFERENCES institution.institution(id),
    curriculum_id           uuid        NOT NULL REFERENCES institution.curriculum(id),
    programme_id            uuid        NOT NULL REFERENCES institution.programme(id),
    version_no              integer     NOT NULL CHECK (version_no >= 1),
    supersedes_version_id   uuid        REFERENCES institution.curriculum_version(id),
    change_summary          text,
    effective_from          date        NOT NULL,
    -- Cohort applicability: only cohorts admitted from this academic-year label follow it
    -- (existing cohorts never migrate automatically, L4-B-024-BH-006).
    applies_from_cohort     text,
    term_structure          text        NOT NULL CHECK (term_structure IN ('semester', 'trimester', 'annual')),
    term_count              integer     NOT NULL CHECK (term_count BETWEEN 1 AND 24),
    credit_framework        text        NOT NULL CHECK (credit_framework IN ('nep2020', 'cbcs', 'custom')),
    min_total_credits       numeric(5,1) CHECK (min_total_credits >= 0),
    min_credits_per_term    numeric(4,1) CHECK (min_credits_per_term >= 0),
    max_credits_per_term    numeric(4,1) CHECK (max_credits_per_term >= 0),
    -- NEP 2020 alignment.
    nep_aligned             boolean     NOT NULL DEFAULT false,
    nheqf_level             numeric(3,1) CHECK (nheqf_level BETWEEN 4.5 AND 8),
    multiple_entry_exit     boolean     NOT NULL DEFAULT false,
    abc_enabled             boolean     NOT NULL DEFAULT false,   -- Academic Bank of Credits
    exit_awards             jsonb       NOT NULL DEFAULT '[]',     -- [{afterTerms, award, minCredits}]
    status                  text        NOT NULL DEFAULT 'draft' CHECK (status IN (
                                'draft', 'in_review', 'published', 'superseded', 'retired')),
    process_instance_id     text,
    status_reason           text,
    published_at            timestamptz,
    retired_at              timestamptz,
    version                 bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT curriculum_version_no_unique UNIQUE (curriculum_id, version_no)
);
CREATE UNIQUE INDEX curriculum_version_one_published ON institution.curriculum_version (curriculum_id)
    WHERE status = 'published';
CREATE UNIQUE INDEX curriculum_version_one_open ON institution.curriculum_version (curriculum_id)
    WHERE status IN ('draft', 'in_review');

-- Course slots of a version (static composition; D01 owns term offerings).
CREATE TABLE institution.curriculum_component (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    curriculum_version_id uuid        NOT NULL REFERENCES institution.curriculum_version(id),
    term_no               integer     NOT NULL CHECK (term_no >= 1),
    slot_code             text        NOT NULL,
    title                 text        NOT NULL,
    category              text        NOT NULL CHECK (category IN (
                              'major_core', 'major_elective', 'minor', 'multidisciplinary',
                              'ability_enhancement', 'skill_enhancement', 'value_added',
                              'internship', 'research_project', 'open_elective')),
    credits               numeric(4,1) NOT NULL CHECK (credits > 0),
    lecture_hours         integer     NOT NULL DEFAULT 0 CHECK (lecture_hours >= 0),
    tutorial_hours        integer     NOT NULL DEFAULT 0 CHECK (tutorial_hours >= 0),
    practical_hours       integer     NOT NULL DEFAULT 0 CHECK (practical_hours >= 0),
    basket_code           text,
    course_code           text,
    sort_order            integer     NOT NULL DEFAULT 0,
    CONSTRAINT curriculum_component_slot_unique UNIQUE (curriculum_version_id, slot_code)
);

-- ---------------------------------------------------------------------------------------------
-- School academic model (classes / grade levels, sections, subjects) — not a programme.
-- ---------------------------------------------------------------------------------------------
CREATE TABLE institution.school_academic_model (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL REFERENCES institution.institution(id),
    name                text        NOT NULL,
    board               text        CHECK (board ~ '^[a-z0-9_]{2,32}$'),
    nep_stage_aligned   boolean     NOT NULL DEFAULT true,   -- 5+3+3+4 stages
    version_no          integer     NOT NULL CHECK (version_no >= 1),
    supersedes_id       uuid        REFERENCES institution.school_academic_model(id),
    effective_from      date,
    status              text        NOT NULL DEFAULT 'draft' CHECK (status IN (
                            'draft', 'in_review', 'published', 'superseded', 'retired')),
    process_instance_id text,
    status_reason       text,
    published_at        timestamptz,
    retired_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT school_model_version_unique UNIQUE (tenant_id, institution_id, version_no)
);
CREATE UNIQUE INDEX school_model_one_published ON institution.school_academic_model (institution_id)
    WHERE status = 'published';
CREATE UNIQUE INDEX school_model_one_open ON institution.school_academic_model (institution_id)
    WHERE status IN ('draft', 'in_review');

CREATE TABLE institution.school_grade_level (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    school_model_id   uuid        NOT NULL REFERENCES institution.school_academic_model(id),
    key               text        NOT NULL CHECK (key ~ '^[a-z0-9][a-z0-9_-]{0,15}$'),
    name              text        NOT NULL,
    nep_stage         text        CHECK (nep_stage IN ('foundational', 'preparatory', 'middle', 'secondary')),
    sections          text[]      NOT NULL DEFAULT '{}',
    subjects          jsonb       NOT NULL DEFAULT '[]',   -- [{code, name, kind}]
    sort_order        integer     NOT NULL,
    CONSTRAINT school_grade_level_key_unique UNIQUE (school_model_id, key)
);

-- ---------------------------------------------------------------------------------------------
-- Immutability of published content (L4-B-023-BH-006, L4-B-024-BH-001). Status bookkeeping
-- (status, status_reason, published_at, retired_at, process_instance_id, version, updated_at)
-- may still move a published row to superseded / retired; content columns may not.
-- ---------------------------------------------------------------------------------------------
CREATE FUNCTION institution.forbid_published_content_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    bookkeeping text[] := ARRAY['status', 'status_reason', 'published_at', 'retired_at',
                                'process_instance_id', 'version', 'updated_at'];
    o jsonb;
    n jsonb;
    k text;
BEGIN
    IF OLD.status IN ('draft', 'in_review') THEN
        RETURN NEW;
    END IF;
    o := to_jsonb(OLD);
    n := to_jsonb(NEW);
    FOREACH k IN ARRAY bookkeeping LOOP
        o := o - k;
        n := n - k;
    END LOOP;
    IF o IS DISTINCT FROM n THEN
        RAISE EXCEPTION 'published % % is immutable', TG_TABLE_NAME, OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER academic_structure_immutable BEFORE UPDATE ON institution.academic_structure
    FOR EACH ROW EXECUTE FUNCTION institution.forbid_published_content_change();
CREATE TRIGGER curriculum_version_immutable BEFORE UPDATE ON institution.curriculum_version
    FOR EACH ROW EXECUTE FUNCTION institution.forbid_published_content_change();
CREATE TRIGGER school_model_immutable BEFORE UPDATE ON institution.school_academic_model
    FOR EACH ROW EXECUTE FUNCTION institution.forbid_published_content_change();

-- Child rows may only change while their parent is a draft.
CREATE FUNCTION institution.forbid_published_child_change() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    parent_status text;
    row_ jsonb := to_jsonb(COALESCE(NEW, OLD));
BEGIN
    EXECUTE format('SELECT status FROM institution.%I WHERE id = $1', TG_ARGV[0])
        INTO parent_status USING (row_ ->> TG_ARGV[1])::uuid;
    IF parent_status IS NOT NULL AND parent_status <> 'draft' THEN
        RAISE EXCEPTION 'content of % is immutable once submitted (parent status %)',
            TG_TABLE_NAME, parent_status USING ERRCODE = 'check_violation';
    END IF;
    RETURN COALESCE(NEW, OLD);
END $$;

CREATE TRIGGER academic_structure_node_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON institution.academic_structure_node
    FOR EACH ROW EXECUTE FUNCTION institution.forbid_published_child_change('academic_structure', 'academic_structure_id');
CREATE TRIGGER curriculum_component_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON institution.curriculum_component
    FOR EACH ROW EXECUTE FUNCTION institution.forbid_published_child_change('curriculum_version', 'curriculum_version_id');
CREATE TRIGGER school_grade_level_immutable
    BEFORE INSERT OR UPDATE OR DELETE ON institution.school_grade_level
    FOR EACH ROW EXECUTE FUNCTION institution.forbid_published_child_change('school_academic_model', 'school_model_id');
