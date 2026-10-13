-- 4C — Course resources and library links (plan line "resources"; contract §1 decision).
--
-- A resource belongs to an offering (offering-wide) or to one section. Faculty manage resources
-- of sections they actively teach; students see published resources of the sections they are
-- seated in (and offering-wide resources of those offerings). Links only: the library catalogue
-- (Resources J, Batch 8) is out of scope.

CREATE TABLE academic.course_resource (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid        NOT NULL,
    offering_id    uuid        NOT NULL REFERENCES academic.course_offering (id),
    section_id     uuid        REFERENCES academic.offering_section (id),
    title          text        NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 200),
    kind           text        NOT NULL CHECK (kind IN ('document', 'link', 'video', 'reading', 'library_item')),
    url            text        NOT NULL CHECK (url ~* '^https?://' AND length(url) <= 2000),
    description    text,
    status         text        NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'withdrawn')),
    status_reason  text,
    created_by     text,
    published_at   timestamptz,
    version        bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX course_resource_offering ON academic.course_resource (tenant_id, offering_id, status);
CREATE INDEX course_resource_section ON academic.course_resource (tenant_id, section_id, status);
CREATE TRIGGER course_resource_no_delete BEFORE DELETE ON academic.course_resource
    FOR EACH ROW EXECUTE FUNCTION academic.forbid_history_change();
