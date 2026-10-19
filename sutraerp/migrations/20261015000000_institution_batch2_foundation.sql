-- Batch 2 foundation (contract doc docs/batch2-contracts.md §3).
--
-- Adds the institution lifecycle status that every Batch 2 slice reads through
-- `sutra_institution::shared::guard` (downstream structure is read-only while the institution
-- is suspended or archived). Lifecycle transitions themselves are slice 2A's.
--
-- Reserved migration ranges (one file per change, never edit an applied file):
--   2A  20261015000001 .. 20261015000009
--   2B  20261015000010 .. 20261015000019
--   2C  20261015000020 .. 20261015000029
ALTER TABLE institution.institution
    ADD COLUMN status text NOT NULL DEFAULT 'draft'
        CONSTRAINT institution_status_check CHECK (status IN (
            'draft', 'configured', 'pending_approval', 'returned', 'rejected',
            'active', 'suspended', 'archived')),
    ADD COLUMN status_changed_at timestamptz NOT NULL DEFAULT now();

-- Institutions registered before Batch 2 were operational under the Batch 0/1 semantics.
UPDATE institution.institution SET status = 'active';
