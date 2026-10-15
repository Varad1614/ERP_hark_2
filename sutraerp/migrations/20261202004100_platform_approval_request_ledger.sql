-- Part 1 · Stage 3 (S3-appr) — approval request ledger for separation of duties (DEF-X-002).
--
-- Every module's `request_approval` records who asked for a governed decision (the authenticated
-- user of the request; NULL for background work) right after the workflow process started.
-- Every decision endpoint reads the request it decides (`sutra_persistence::approval::
-- guard_decision*`) and refuses (403) a decision by one of its requesters or by a system actor.
-- Append-only: no runtime role may change or remove a request once recorded.

CREATE TABLE platform.approval_request (
    id                      uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid        NOT NULL,
    process_definition_key  text        NOT NULL,
    business_key            text        NOT NULL,
    -- `<subject>` / `<version>` of the deterministic business key `<aggregate>:<subject>:v<version>`.
    subject_id              text        NOT NULL,
    subject_version         bigint,
    process_instance_id     text        NOT NULL,
    requested_by            text,
    requested_at            timestamptz NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX approval_request_subject_idx
    ON platform.approval_request (tenant_id, subject_id, requested_at DESC);
CREATE INDEX approval_request_process_idx
    ON platform.approval_request (tenant_id, process_instance_id);

CREATE FUNCTION platform.approval_request_immutable() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'platform.approval_request is append-only';
END;
$$;

CREATE TRIGGER approval_request_no_update_delete
    BEFORE UPDATE OR DELETE ON platform.approval_request
    FOR EACH ROW EXECUTE FUNCTION platform.approval_request_immutable();

-- Shared platform service used by every module (like platform.audit_log): record + read only.
DO $$
DECLARE
    s text;
BEGIN
    FOREACH s IN ARRAY ARRAY['platform', 'institution', 'student', 'academic', 'people',
                             'finance', 'campus', 'services', 'exams', 'resources'] LOOP
        IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_' || s) THEN
            EXECUTE format('GRANT SELECT, INSERT ON platform.approval_request TO %I', 'sutra_' || s);
            EXECUTE format('REVOKE UPDATE, DELETE, TRUNCATE ON platform.approval_request FROM %I', 'sutra_' || s);
        END IF;
    END LOOP;
END $$;
