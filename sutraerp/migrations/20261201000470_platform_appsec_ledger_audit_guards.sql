-- Part 1 · Stage 3 (appsec) — financial-integrity and audit-evidence guards the database enforces
-- whatever role writes (docs/readiness/security/appsec.md: SEC-P1-APPSEC-001 / -002).
--
-- 1. TRUNCATE guard (SEC-P1-APPSEC-001). The append-only ledger and audit tables already refuse
--    UPDATE / DELETE with row-level triggers, and the runtime roles hold no UPDATE / DELETE /
--    TRUNCATE privilege on them. Row-level triggers do not fire on TRUNCATE, so the schema owner
--    (the migrating role, an operator console, a mistaken maintenance script) could still wipe the
--    general ledger or the audit trail in one statement. A statement-level BEFORE TRUNCATE trigger
--    closes that path for every role except a superuser who disables triggers deliberately.
--
-- 2. Posted manual journals are immutable (SEC-P1-APPSEC-002). `finance.manual_journal` and its
--    lines were guarded only by the application ("only drafts can be edited"): any UPDATE could
--    rewrite the amount, posting date or description of a posted / reversed journal, or delete /
--    rewrite its lines, so the journal no longer matched the ledger entry it produced. Now:
--    * content (number, date, kind, description, currency, amount, requester) changes only while
--      the journal is a draft;
--    * the linked ledger entries are write-once (NULL → value), the status follows the lifecycle
--      once out of draft (a posted journal may only become reversed; a reversed one never changes);
--    * the decision record (decided by / at / reason) of a posted journal is frozen once its
--      ledger entry is linked;
--    * a journal that left draft cannot be deleted; its lines can be inserted, rewritten or
--      deleted only while the journal is a draft.

-- ------------------------------------------------------------------ 1. TRUNCATE guard
CREATE FUNCTION platform.forbid_truncate() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION '%.% is append-only evidence and cannot be truncated', TG_TABLE_SCHEMA, TG_TABLE_NAME
        USING ERRCODE = 'insufficient_privilege';
END;
$$;

DO $$
DECLARE
    t text;
    tables text[] := ARRAY[
        'platform.audit_log', 'platform.audit_chain_entry', 'platform.audit_anchor',
        'finance.journal_entry', 'finance.journal_line', 'finance.status_history',
        'finance.charge_line', 'finance.trial_balance', 'finance.gateway_webhook_event'];
BEGIN
    FOREACH t IN ARRAY tables LOOP
        EXECUTE format(
            'CREATE TRIGGER no_truncate BEFORE TRUNCATE ON %s FOR EACH STATEMENT '
            'EXECUTE FUNCTION platform.forbid_truncate()', t);
    END LOOP;
END $$;

-- ------------------------------------------------------------------ 2. manual journals
CREATE FUNCTION finance.guard_manual_journal() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        IF OLD.status <> 'draft' THEN
            RAISE EXCEPTION 'manual journal % is % and cannot be deleted', OLD.journal_number, OLD.status
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN OLD;
    END IF;
    IF OLD.status = 'draft' THEN
        RETURN NEW;
    END IF;
    IF NEW.tenant_id IS DISTINCT FROM OLD.tenant_id
       OR NEW.institution_id IS DISTINCT FROM OLD.institution_id
       OR NEW.journal_number IS DISTINCT FROM OLD.journal_number
       OR NEW.posting_date IS DISTINCT FROM OLD.posting_date
       OR NEW.entry_kind IS DISTINCT FROM OLD.entry_kind
       OR NEW.description IS DISTINCT FROM OLD.description
       OR NEW.currency IS DISTINCT FROM OLD.currency
       OR NEW.total_amount IS DISTINCT FROM OLD.total_amount
       OR NEW.requested_by IS DISTINCT FROM OLD.requested_by
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'manual journal % is %; its content is immutable', OLD.journal_number, OLD.status
            USING ERRCODE = 'check_violation';
    END IF;
    IF (OLD.journal_entry_id IS NOT NULL AND NEW.journal_entry_id IS DISTINCT FROM OLD.journal_entry_id)
       OR (OLD.reversal_entry_id IS NOT NULL AND NEW.reversal_entry_id IS DISTINCT FROM OLD.reversal_entry_id) THEN
        RAISE EXCEPTION 'manual journal % ledger links are write-once', OLD.journal_number
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.status = 'posted' AND NEW.status NOT IN ('posted', 'reversed') THEN
        RAISE EXCEPTION 'posted manual journal % can only be reversed (not %)', OLD.journal_number, NEW.status
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.status = 'reversed' AND NEW.status <> 'reversed' THEN
        RAISE EXCEPTION 'reversed manual journal % is final', OLD.journal_number
            USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.status IN ('posted', 'reversed') AND OLD.journal_entry_id IS NOT NULL
       AND (NEW.decided_by IS DISTINCT FROM OLD.decided_by
            OR NEW.decided_at IS DISTINCT FROM OLD.decided_at
            OR NEW.decision_reason IS DISTINCT FROM OLD.decision_reason) THEN
        RAISE EXCEPTION 'manual journal % decision record is immutable once posted', OLD.journal_number
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER manual_journal_guard BEFORE UPDATE OR DELETE ON finance.manual_journal
    FOR EACH ROW EXECUTE FUNCTION finance.guard_manual_journal();

CREATE FUNCTION finance.guard_manual_journal_line() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    st text;
    num text;
BEGIN
    SELECT status, journal_number INTO st, num FROM finance.manual_journal
     WHERE id = CASE WHEN TG_OP = 'DELETE' THEN OLD.manual_journal_id ELSE NEW.manual_journal_id END;
    IF st IS DISTINCT FROM 'draft' THEN
        RAISE EXCEPTION 'manual journal % is %; its lines are immutable', num, st
            USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND NEW.manual_journal_id IS DISTINCT FROM OLD.manual_journal_id THEN
        SELECT status, journal_number INTO st, num FROM finance.manual_journal WHERE id = OLD.manual_journal_id;
        IF st IS DISTINCT FROM 'draft' THEN
            RAISE EXCEPTION 'manual journal % is %; its lines are immutable', num, st
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER manual_journal_line_guard BEFORE INSERT OR UPDATE OR DELETE ON finance.manual_journal_line
    FOR EACH ROW EXECUTE FUNCTION finance.guard_manual_journal_line();
