-- 4B — Syllabus coverage (L2-D04, L3-D-026). A coverage record per section moves
-- draft → submitted → published; a published record is immutable and a later change is a new
-- record that supersedes it (`supersedes_id`).

CREATE TABLE academic.syllabus_coverage (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    institution_id    uuid        NOT NULL,
    offering_id       uuid        NOT NULL REFERENCES academic.course_offering(id),
    section_id        uuid        NOT NULL REFERENCES academic.offering_section(id),
    summary           text,
    status            text        NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'submitted', 'published')),
    supersedes_id     uuid        REFERENCES academic.syllabus_coverage(id),
    submitted_by      text,
    published_by      text,
    published_at      timestamptz,
    version           bigint      NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX syllabus_coverage_one_open ON academic.syllabus_coverage (section_id)
    WHERE status IN ('draft', 'submitted');
CREATE INDEX syllabus_coverage_section ON academic.syllabus_coverage (tenant_id, section_id, created_at);

CREATE TABLE academic.syllabus_topic (
    id                    uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id             uuid        NOT NULL,
    coverage_id           uuid        NOT NULL REFERENCES academic.syllabus_coverage(id),
    position              integer     NOT NULL,
    title                 text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 300),
    learning_outcome_ref  text,
    session_id            uuid        REFERENCES academic.teaching_session(id),
    status                text        NOT NULL CHECK (status IN ('planned', 'started', 'completed', 'deferred', 'cancelled')),
    notes                 text,
    UNIQUE (coverage_id, position)
);

-- Topics of a published coverage record are immutable (L4-D-026-BH-007).
CREATE FUNCTION academic.syllabus_topic_guard() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    st text;
BEGIN
    SELECT status INTO st FROM academic.syllabus_coverage
     WHERE id = COALESCE(NEW.coverage_id, OLD.coverage_id);
    IF st = 'published' THEN
        RAISE EXCEPTION 'topics of a published syllabus coverage record are immutable';
    END IF;
    RETURN COALESCE(NEW, OLD);
END;
$$;
CREATE TRIGGER syllabus_topic_immutable BEFORE INSERT OR UPDATE OR DELETE ON academic.syllabus_topic
    FOR EACH ROW EXECUTE FUNCTION academic.syllabus_topic_guard();
