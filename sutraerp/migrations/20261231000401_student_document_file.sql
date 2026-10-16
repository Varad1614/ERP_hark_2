-- Part 2 · Stage 4.2 (S4-docs): the stored file of an admission document / a document on the
-- student's record.
--
-- `file_ref` stays the free-text reference it always was (paper originals, external DMS);
-- `document_object_id` points at the file kept by the platform document service
-- (platform.document_object, S3-compatible storage). Access to the file is decided by the owning
-- record (admission staff, the student for documents on his / her own record) and then
-- re-checked by the document service (tenant, institution and owner must match). No foreign key
-- across module schemas (ADR-L5-0036 §3.2); the document service is the authority on the id.
-- When an admission creates its student, verified documents move onto the record with the same
-- document_object_id, so the student can retrieve the files that were verified.

ALTER TABLE student.admission_document ADD COLUMN document_object_id uuid;
ALTER TABLE student.student_document ADD COLUMN document_object_id uuid;

-- One file per admission document (a new upload is a new document that supersedes it).
CREATE UNIQUE INDEX admission_document_object_uq
    ON student.admission_document (document_object_id) WHERE document_object_id IS NOT NULL;

-- A file, once attached, is never swapped or detached.
CREATE FUNCTION student.document_object_attach_once() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.document_object_id IS NOT NULL
       AND NEW.document_object_id IS DISTINCT FROM OLD.document_object_id THEN
        RAISE EXCEPTION 'the stored file of document % cannot be replaced or removed', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_attach_once';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER admission_document_object_attach_once BEFORE UPDATE ON student.admission_document
    FOR EACH ROW EXECUTE FUNCTION student.document_object_attach_once();
CREATE TRIGGER student_document_object_attach_once BEFORE UPDATE ON student.student_document
    FOR EACH ROW EXECUTE FUNCTION student.document_object_attach_once();
