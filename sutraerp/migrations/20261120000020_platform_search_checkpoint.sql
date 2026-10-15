-- Batch 9 · slice 9C — OpenSearch projection checkpoints (docs/batch9-contracts.md §8).
--
-- One row per projection (`students`, `staff`, `library-catalogue`). The projector job
-- `search_project` reads platform.outbox in id order (UUIDv7 = enqueue order) after
-- `last_outbox_id` and moves the checkpoint in the same transaction as its advisory lock.
-- The rebuild job `search_rebuild` re-reads every document through the owning module's
-- SearchSource port when a projection has no row yet (first start) or when
-- `rebuild_requested_at` is set (operators / the DR restore: UPDATE … SET rebuild_requested_at = now()).
-- Documents are written with external versions, so a replay after a restore is idempotent.
CREATE TABLE platform.search_checkpoint (
    projection            text        PRIMARY KEY
        CHECK (projection ~ '^[a-z][a-z-]{1,62}$'),
    last_outbox_id        uuid,
    -- created_at of the last projected outbox row.
    last_event_at         timestamptz,
    rebuild_requested_at  timestamptz,
    last_rebuild_at       timestamptz,
    -- Documents written by the last rebuild (the DR drill compares it with the source count).
    last_rebuild_count    bigint      CHECK (last_rebuild_count IS NULL OR last_rebuild_count >= 0),
    updated_at            timestamptz NOT NULL DEFAULT now()
);

COMMENT ON TABLE platform.search_checkpoint IS
    '9C: outbox position of each OpenSearch projection + rebuild requests (search_project / search_rebuild jobs).';

-- The platform runtime role reads and moves checkpoints (the platform schema default privileges
-- already grant this; restated so the intent is explicit).
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sutra_platform') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON platform.search_checkpoint TO sutra_platform;
    END IF;
END $$;
