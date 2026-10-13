-- 4B — Timetable (L2-D03, L3-D-020…022; docs/batch4-contracts.md §3.3).
--
-- Scheduling requirements per section → a timetable version (planning → draft → validated →
-- approved → published → active → revised/retired → archived) with weekly slots. A revision is a
-- distinct timetable row (`revision_of_id`, `version_no + 1`); the prior version is never edited.
-- Rooms are free text until Campus (Batch 8): `room_label` + nullable `teaching_space_id`.

CREATE TABLE academic.scheduling_requirement (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    institution_id    uuid        NOT NULL,
    academic_term_id  uuid        NOT NULL,
    offering_id       uuid        NOT NULL REFERENCES academic.course_offering(id),
    section_id        uuid        NOT NULL REFERENCES academic.offering_section(id),
    sessions_per_week smallint    NOT NULL CHECK (sessions_per_week BETWEEN 1 AND 20),
    session_minutes   smallint    NOT NULL CHECK (session_minutes BETWEEN 15 AND 480),
    -- ISO days (1 = Monday) the section may be scheduled on; empty = Monday–Friday.
    preferred_days    smallint[]  NOT NULL DEFAULT '{}',
    room_label        text,
    teaching_space_id uuid,
    notes             text,
    status            text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'withdrawn')),
    version           bigint      NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX scheduling_requirement_one_live ON academic.scheduling_requirement (section_id)
    WHERE status = 'active';
CREATE INDEX scheduling_requirement_term ON academic.scheduling_requirement (tenant_id, institution_id, academic_term_id);

CREATE TABLE academic.timetable (
    id                  uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid        NOT NULL,
    institution_id      uuid        NOT NULL,
    academic_term_id    uuid        NOT NULL,
    name                text        NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    version_no          integer     NOT NULL DEFAULT 1 CHECK (version_no >= 1),
    revision_of_id      uuid        REFERENCES academic.timetable(id),
    status              text        NOT NULL DEFAULT 'planning' CHECK (status IN (
        'planning', 'draft', 'validated', 'approved', 'published', 'active', 'frozen', 'revised',
        'retired', 'archived')),
    status_reason       text,
    status_changed_at   timestamptz NOT NULL DEFAULT now(),
    -- Last validation outcome: {"valid": bool, "conflicts": [...], "validatedAt": ...}.
    validation_report   jsonb,
    -- Approval requested and waiting for a decision (AwaitDecision).
    pending_decision    boolean     NOT NULL DEFAULT false,
    process_instance_id text,
    effective_from      date,
    published_at        timestamptz,
    activated_at        timestamptz,
    retired_at          timestamptz,
    version             bigint      NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
-- Exactly one operational timetable per institution and term (L4-D-020-BH-009).
CREATE UNIQUE INDEX timetable_one_active ON academic.timetable (tenant_id, institution_id, academic_term_id)
    WHERE status IN ('active', 'frozen');
CREATE INDEX timetable_scope ON academic.timetable (tenant_id, institution_id, academic_term_id, status);
CREATE TRIGGER timetable_no_delete BEFORE DELETE ON academic.timetable
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();

CREATE TABLE academic.timetable_slot (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    institution_id    uuid        NOT NULL,
    timetable_id      uuid        NOT NULL REFERENCES academic.timetable(id),
    offering_id       uuid        NOT NULL REFERENCES academic.course_offering(id),
    section_id        uuid        NOT NULL REFERENCES academic.offering_section(id),
    day_of_week       smallint    NOT NULL CHECK (day_of_week BETWEEN 1 AND 7),
    starts_at         time        NOT NULL,
    ends_at           time        NOT NULL,
    room_label        text,
    teaching_space_id uuid,
    created_at        timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX timetable_slot_timetable ON academic.timetable_slot (timetable_id, day_of_week, starts_at);
CREATE INDEX timetable_slot_section ON academic.timetable_slot (section_id);

-- Slots of a timetable past `draft` are immutable (published versions never change, L4-D-021-BH-003).
CREATE FUNCTION academic.timetable_slot_guard() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    st text;
BEGIN
    SELECT status INTO st FROM academic.timetable
     WHERE id = COALESCE(NEW.timetable_id, OLD.timetable_id);
    IF st NOT IN ('planning', 'draft') THEN
        RAISE EXCEPTION 'slots of a % timetable are immutable', st;
    END IF;
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER timetable_slot_immutable BEFORE INSERT OR UPDATE OR DELETE ON academic.timetable_slot
    FOR EACH ROW EXECUTE FUNCTION academic.timetable_slot_guard();

CREATE TABLE academic.timetable_revision (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    institution_id        uuid        NOT NULL,
    -- The operational timetable being revised and the distinct revised version.
    timetable_id          uuid        NOT NULL REFERENCES academic.timetable(id),
    revised_timetable_id  uuid        NOT NULL REFERENCES academic.timetable(id),
    reason                text        NOT NULL CHECK (length(btrim(reason)) > 0),
    affected_section_ids  uuid[]      NOT NULL DEFAULT '{}',
    status                text        NOT NULL DEFAULT 'requested' CHECK (status IN (
        'requested', 'validated', 'approved', 'applied', 'rejected', 'withdrawn')),
    status_reason         text,
    process_instance_id   text,
    version               bigint      NOT NULL DEFAULT 1,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX timetable_revision_one_open ON academic.timetable_revision (timetable_id)
    WHERE status IN ('requested', 'validated', 'approved');
CREATE INDEX timetable_revision_revised ON academic.timetable_revision (revised_timetable_id);
