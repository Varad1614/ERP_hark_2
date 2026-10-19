-- Batch 1C — outbox relay to NATS JetStream (ADR-L5-0007 / ADR-L5-0010 / ADR-L5-0022).
--
-- Relay protocol (sutra-persistence::outbox_relay):
-- * claim:   a short transaction selects due rows (published_at IS NULL, dead_lettered_at IS NULL,
--            next_attempt_at <= now()) in created_at order with FOR UPDATE SKIP LOCKED and leases
--            them by stamping claim_token and pushing next_attempt_at to now() + lease. A relay
--            that crashes mid-batch therefore releases its rows when the lease runs out.
-- * publish: each message goes to JetStream with Nats-Msg-Id = id (broker-side de-duplication
--            covers a re-publish after a lost ack or an expired lease).
-- * mark:    only after the publish ack: published_at = now(). A failure increments attempts,
--            records last_error and schedules next_attempt_at with exponential backoff + jitter;
--            after the configured maximum attempts (or a permanent error) the row is
--            dead-lettered and left for an operator. Marks are guarded by claim_token.

ALTER TABLE platform.outbox
    ADD COLUMN claim_token      uuid,
    ADD COLUMN last_attempt_at  timestamptz,
    ADD COLUMN dead_lettered_at timestamptz,
    ADD CONSTRAINT outbox_published_xor_dead_lettered
        CHECK (published_at IS NULL OR dead_lettered_at IS NULL),
    ADD CONSTRAINT outbox_attempts_non_negative CHECK (attempts >= 0);

-- Due rows only: dead-lettered rows no longer occupy the relay's index.
DROP INDEX platform.outbox_pending_idx;
CREATE INDEX outbox_pending_idx ON platform.outbox (next_attempt_at, created_at)
    WHERE published_at IS NULL AND dead_lettered_at IS NULL;

-- Per-aggregate ordering: an event is held back while an earlier event of the same
-- (tenant, subject) is still pending and not due (in backoff or leased by another relay).
CREATE INDEX outbox_pending_subject_idx ON platform.outbox (tenant_id, subject, created_at)
    WHERE published_at IS NULL AND dead_lettered_at IS NULL AND subject IS NOT NULL;

-- Retention sweeper: published rows older than the retention window are purged.
CREATE INDEX outbox_published_at_idx ON platform.outbox (published_at)
    WHERE published_at IS NOT NULL;

-- Operator view of the dead-letter set.
CREATE INDEX outbox_dead_lettered_idx ON platform.outbox (dead_lettered_at)
    WHERE dead_lettered_at IS NOT NULL;

COMMENT ON COLUMN platform.outbox.claim_token IS 'Lease token of the relay currently publishing the row (NULL when idle).';
COMMENT ON COLUMN platform.outbox.last_attempt_at IS 'When the relay last tried to publish the row.';
COMMENT ON COLUMN platform.outbox.dead_lettered_at IS 'Set after the maximum attempts or a permanent error; never purged automatically.';
