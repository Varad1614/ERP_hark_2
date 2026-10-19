-- Part 1 · Stage 3 (S3-config, DEF-J-004): an approved supplier's bank details change only through
-- an independently approved bank-change request.
--
-- `PATCH /suppliers/{id}` used to overwrite bank_account_* / bank_ifsc of an active supplier with
-- no approval and an audit entry that recorded only the supplier number — the classic payment
-- redirection fraud. Now `POST /suppliers/{id}/bank-change` stores the requested values in the
-- pending_bank_* columns (the live details are untouched and keep being used), starts
-- SUPPLIER_BANK_CHANGE_APPROVAL and notifies the approvers; `POST /suppliers/{id}/bank-change/
-- decision` by another user with an approve permission promotes (or discards) them.
--
-- The trigger below makes the rule hold for every writer, not just the HTTP handlers: once a
-- supplier has been approved (approved_at set), its bank columns may only change by promoting the
-- pending values in the same UPDATE that clears them.

ALTER TABLE resources.supplier
    ADD COLUMN pending_bank_account_name    text,
    ADD COLUMN pending_bank_account_number  text,
    ADD COLUMN pending_bank_ifsc            text
        CHECK (pending_bank_ifsc IS NULL OR pending_bank_ifsc ~ '^[A-Z]{4}0[A-Z0-9]{6}$'),
    ADD COLUMN bank_change_reason           text,
    ADD COLUMN bank_change_requested_by     text,
    ADD COLUMN bank_change_requested_at     timestamptz,
    ADD COLUMN bank_change_process_instance_id text,
    -- One id per request: the status history (`supplier_bank_change`) and the maker-checker rule
    -- are keyed by it, so an earlier request's maker is not barred from deciding a later one.
    ADD COLUMN bank_change_request_id       uuid,
    ADD CONSTRAINT supplier_bank_change_complete CHECK (
        (pending_bank_account_number IS NULL) = (pending_bank_ifsc IS NULL)
        AND (pending_bank_account_number IS NULL) = (bank_change_requested_at IS NULL)
        AND (pending_bank_account_number IS NULL) = (bank_change_request_id IS NULL)
    );

CREATE FUNCTION resources.guard_supplier_bank_details() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.approved_at IS NULL THEN
        RETURN NEW; -- not yet approved: the details are part of the registration being approved
    END IF;
    IF (NEW.bank_account_name, NEW.bank_account_number, NEW.bank_ifsc)
       IS NOT DISTINCT FROM (OLD.bank_account_name, OLD.bank_account_number, OLD.bank_ifsc) THEN
        RETURN NEW;
    END IF;
    IF OLD.pending_bank_account_number IS NOT NULL
       AND NEW.pending_bank_account_number IS NULL
       AND (NEW.bank_account_name, NEW.bank_account_number, NEW.bank_ifsc)
           IS NOT DISTINCT FROM (OLD.pending_bank_account_name, OLD.pending_bank_account_number,
                                 OLD.pending_bank_ifsc) THEN
        RETURN NEW; -- an approved bank-change request is being applied
    END IF;
    RAISE EXCEPTION 'bank details of approved supplier % change only through an approved bank-change request', OLD.number
        USING ERRCODE = 'check_violation', CONSTRAINT = 'supplier_bank_change_requires_approval';
END;
$$;

CREATE TRIGGER supplier_bank_details_guard BEFORE UPDATE ON resources.supplier
    FOR EACH ROW EXECUTE FUNCTION resources.guard_supplier_bank_details();
