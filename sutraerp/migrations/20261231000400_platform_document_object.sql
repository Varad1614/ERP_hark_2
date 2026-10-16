-- Part 2 · Stage 4.2 (S4-docs): document storage metadata.
--
-- Files live in S3-compatible object storage (MinIO on the college's own server, or AWS S3);
-- this table is the system of record for them: owner, sanitised name, type detected from the
-- content, size, SHA-256 (verified on upload and on every read), the tenant-rooted storage key,
-- retention and legal hold. Rows are never physically deleted: a deletion stamps deleted_at
-- (only after retention, never under legal hold) and the sweeper stamps purged_at once the
-- object is gone from storage. Forced row-level security (tenant + institution pinning) comes
-- from platform.apply_tenant_rls() at the end.

CREATE TABLE platform.document_object (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid,
    owner_type         text        NOT NULL CHECK (owner_type ~ '^[a-z][a-z0-9_]{1,47}$'),
    owner_id           uuid        NOT NULL,
    filename           text        NOT NULL CHECK (length(filename) BETWEEN 1 AND 120
                                                   AND filename !~ '[/\\[:cntrl:]]'),
    content_type       text        NOT NULL CHECK (content_type IN (
                                       'application/pdf', 'image/png', 'image/jpeg')),
    size_bytes         bigint      NOT NULL CHECK (size_bytes > 0),
    sha256             text        NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    -- <tenant>/<institution | tenant>/<owner_type>/<owner_id>/<id>; no file name in the key.
    storage_key        text        NOT NULL UNIQUE,
    storage_backend    text        NOT NULL CHECK (storage_backend IN ('s3')),
    uploaded_by        text,
    uploaded_at        timestamptz NOT NULL DEFAULT now(),
    retention_until    timestamptz NOT NULL,
    legal_hold         boolean     NOT NULL DEFAULT false,
    legal_hold_reason  text,
    legal_hold_by      text,
    legal_hold_at      timestamptz,
    quarantined_at     timestamptz,
    deleted_at         timestamptz,
    deleted_by         text,
    delete_reason      text,
    purged_at          timestamptz,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    CONSTRAINT document_object_key_in_tenant CHECK (storage_key LIKE tenant_id::text || '/%'),
    CONSTRAINT document_object_retention_after_upload CHECK (retention_until >= uploaded_at),
    CONSTRAINT document_object_hold_has_reason CHECK (NOT legal_hold OR legal_hold_reason IS NOT NULL),
    CONSTRAINT document_object_purged_after_delete CHECK (purged_at IS NULL OR deleted_at IS NOT NULL),
    CONSTRAINT document_object_not_deleted_under_hold CHECK (deleted_at IS NULL OR NOT legal_hold)
);

CREATE INDEX document_object_owner_idx
    ON platform.document_object (tenant_id, owner_type, owner_id);
CREATE INDEX document_object_retention_idx
    ON platform.document_object (retention_until) WHERE deleted_at IS NULL AND NOT legal_hold;
CREATE INDEX document_object_purge_idx
    ON platform.document_object (deleted_at) WHERE deleted_at IS NOT NULL AND purged_at IS NULL;

-- Identity and content columns never change; retention only grows; deletion only after the
-- retention and never under legal hold; deleted / purged / quarantined stamps are permanent.
CREATE FUNCTION platform.document_object_guard() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'platform.document_object rows are never deleted (stamp deleted_at)'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_no_delete';
    END IF;
    IF (NEW.id, NEW.tenant_id, NEW.institution_id, NEW.owner_type, NEW.owner_id, NEW.filename,
        NEW.content_type, NEW.size_bytes, NEW.sha256, NEW.storage_key, NEW.storage_backend,
        NEW.uploaded_by, NEW.uploaded_at)
       IS DISTINCT FROM
       (OLD.id, OLD.tenant_id, OLD.institution_id, OLD.owner_type, OLD.owner_id, OLD.filename,
        OLD.content_type, OLD.size_bytes, OLD.sha256, OLD.storage_key, OLD.storage_backend,
        OLD.uploaded_by, OLD.uploaded_at) THEN
        RAISE EXCEPTION 'document % identity and content columns are immutable', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_immutable';
    END IF;
    IF NEW.retention_until < OLD.retention_until THEN
        RAISE EXCEPTION 'document % retention can only be extended', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_retention_only_grows';
    END IF;
    IF (OLD.deleted_at IS NOT NULL AND NEW.deleted_at IS DISTINCT FROM OLD.deleted_at)
       OR (OLD.purged_at IS NOT NULL AND NEW.purged_at IS DISTINCT FROM OLD.purged_at)
       OR (OLD.quarantined_at IS NOT NULL AND NEW.quarantined_at IS DISTINCT FROM OLD.quarantined_at) THEN
        RAISE EXCEPTION 'document % deletion, purge and quarantine stamps are permanent', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_stamps_permanent';
    END IF;
    IF OLD.deleted_at IS NOT NULL AND NEW.legal_hold AND NOT OLD.legal_hold THEN
        RAISE EXCEPTION 'document % is deleted; it cannot be placed on legal hold', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_deleted_no_hold';
    END IF;
    IF OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL THEN
        IF OLD.legal_hold OR NEW.legal_hold THEN
            RAISE EXCEPTION 'document % is under legal hold and cannot be deleted', OLD.id
                USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_legal_hold';
        END IF;
        IF OLD.retention_until > now() THEN
            RAISE EXCEPTION 'document % is under retention until %', OLD.id, OLD.retention_until
                USING ERRCODE = 'check_violation', CONSTRAINT = 'document_object_retention';
        END IF;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER document_object_guard_update BEFORE UPDATE ON platform.document_object
    FOR EACH ROW EXECUTE FUNCTION platform.document_object_guard();
CREATE TRIGGER document_object_guard_delete BEFORE DELETE ON platform.document_object
    FOR EACH ROW EXECUTE FUNCTION platform.document_object_guard();

-- Runtime roles: read, record and stamp; never remove a row.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_platform') THEN
        GRANT SELECT, INSERT, UPDATE ON platform.document_object TO sutra_platform;
        REVOKE DELETE, TRUNCATE ON platform.document_object FROM sutra_platform;
    END IF;
END $$;

COMMENT ON TABLE platform.document_object IS
    'Part 2 S4-docs: metadata of documents kept in S3-compatible object storage (integrity, retention, legal hold).';

SELECT platform.apply_tenant_rls();
