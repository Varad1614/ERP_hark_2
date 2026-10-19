-- Part 1 Stage 2 (Domain I readiness fixes).
--
-- DEF-I-003: releasing a withheld result after the batch is approved needs an explicit release
-- approval (RESULT_APPROVAL) unless it returns unchanged to the approval it already had. The
-- pending release is a governed result decision of the new kind `release`.
ALTER TABLE exams.result_decision DROP CONSTRAINT result_decision_kind_check;
ALTER TABLE exams.result_decision ADD CONSTRAINT result_decision_kind_check
    CHECK (kind IN ('correction', 'withdrawal', 'release'));

-- DEF-I-010: the set of results a publication may publish is frozen when it is authorised;
-- publishing publishes only that set (results approved later need a new publication).
CREATE TABLE exams.publication_snapshot (
    id                   uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid         NOT NULL,
    publication_id       uuid         NOT NULL REFERENCES exams.publication(id),
    result_id            uuid         NOT NULL REFERENCES exams.term_result(id),
    registration_id      uuid         NOT NULL REFERENCES exams.exam_registration(id),
    created_at           timestamptz  NOT NULL DEFAULT now(),
    UNIQUE (publication_id, result_id)
);
CREATE INDEX publication_snapshot_tenant_idx ON exams.publication_snapshot (tenant_id, publication_id);
REVOKE UPDATE, DELETE, TRUNCATE ON exams.publication_snapshot FROM sutra_exams;
CREATE TRIGGER publication_snapshot_append_only BEFORE UPDATE OR DELETE ON exams.publication_snapshot
    FOR EACH ROW EXECUTE FUNCTION exams.forbid_history_change();

-- Publications already authorised before this migration: freeze the set they would have
-- published at this moment (the session's approved results).
INSERT INTO exams.publication_snapshot (tenant_id, publication_id, result_id, registration_id)
SELECT p.tenant_id, p.id, r.id, r.registration_id
FROM exams.publication p
JOIN exams.term_result r ON r.tenant_id = p.tenant_id AND r.session_id = p.session_id
WHERE p.status IN ('authorised', 'ready', 'failed') AND r.status = 'approved'
ON CONFLICT (publication_id, result_id) DO NOTHING;
