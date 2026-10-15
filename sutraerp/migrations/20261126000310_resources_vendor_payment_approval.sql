-- Part 1 · Stage 2 (DEF-J-003): vendor payments need an independent approval before they post.
--
-- Lifecycle: pending_approval (VENDOR_PAYMENT_APPROVAL started on create) → approved | rejected;
-- approved → posted. The old `draft` state is gone: payments drafted before this migration had no
-- approval and are moved to `pending_approval`, so none of them can post without a decision.
-- `approval_requested_by` is the maker (refused as decider, DEF-X-002); `decided_*` record the
-- checker and the decision.

ALTER TABLE resources.vendor_payment DROP CONSTRAINT vendor_payment_status_check;
UPDATE resources.vendor_payment SET status = 'pending_approval', version = version + 1,
    updated_at = now() WHERE status = 'draft';
ALTER TABLE resources.vendor_payment ADD CONSTRAINT vendor_payment_status_check
    CHECK (status IN ('pending_approval', 'approved', 'rejected', 'posted'));
ALTER TABLE resources.vendor_payment ALTER COLUMN status SET DEFAULT 'pending_approval';

ALTER TABLE resources.vendor_payment
    ADD COLUMN process_instance_id   text,
    ADD COLUMN approval_requested_by text,
    ADD COLUMN decided_by            text,
    ADD COLUMN decided_at            timestamptz,
    ADD COLUMN decision_reason       text;

CREATE INDEX vendor_payment_status_idx ON resources.vendor_payment (tenant_id, institution_id, status);
