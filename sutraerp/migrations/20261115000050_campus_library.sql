-- Batch 8 · slice 8C — Domain G library (L2-G05; L3-G-124…139, 149, 152).
-- Borrowing policy versions, memberships, catalogue records, copies, loans, reservations, fines
-- (billed to students through CampusChargePort) and clearance. Persons are stored as
-- (person_kind, person_id); Finance charge ids are plain uuids (validated through the port).

CREATE TABLE campus.library_policy (
    id                      uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id               uuid          NOT NULL,
    institution_id          uuid          NOT NULL,
    name                    text          NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    effective_from          date          NOT NULL,
    loan_days_student       integer       NOT NULL CHECK (loan_days_student BETWEEN 1 AND 365),
    loan_days_staff         integer       NOT NULL CHECK (loan_days_staff BETWEEN 1 AND 365),
    max_loans_student       integer       NOT NULL CHECK (max_loans_student BETWEEN 0 AND 100),
    max_loans_staff         integer       NOT NULL CHECK (max_loans_staff BETWEEN 0 AND 100),
    max_renewals            integer       NOT NULL CHECK (max_renewals BETWEEN 0 AND 20),
    fine_per_day            numeric(14,2) NOT NULL CHECK (fine_per_day >= 0),
    max_fine                numeric(14,2) CHECK (max_fine IS NULL OR max_fine >= 0),
    reservation_hold_days   integer       NOT NULL CHECK (reservation_hold_days BETWEEN 1 AND 60),
    lost_charge_factor      numeric(7,4)  NOT NULL CHECK (lost_charge_factor >= 0),
    status                  text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'active', 'superseded')),
    approval_state          text          NOT NULL DEFAULT 'none' CHECK (approval_state IN ('none', 'pending', 'approved', 'rejected')),
    status_reason           text,
    process_instance_id     text,
    activated_at            timestamptz,
    superseded_by           uuid          REFERENCES campus.library_policy (id),
    version                 bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at              timestamptz   NOT NULL DEFAULT now(),
    updated_at              timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX library_policy_one_active ON campus.library_policy (tenant_id, institution_id) WHERE status = 'active';
CREATE INDEX library_policy_inst ON campus.library_policy (tenant_id, institution_id, status);

CREATE TABLE campus.library_membership (
    id                  uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id           uuid          NOT NULL,
    institution_id      uuid          NOT NULL,
    number              text          NOT NULL CHECK (number ~ '^LM-[0-9]{6,}$'),
    person_kind         text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id           uuid          NOT NULL,
    person_number       text          NOT NULL,
    display_name        text          NOT NULL,
    subject_ref         text,
    status              text          NOT NULL DEFAULT 'active' CHECK (status IN ('requested', 'active', 'suspended', 'closed')),
    status_reason       text,
    -- Administrative borrowing restriction (L3-G-152): a validation condition, not a lifecycle.
    restriction_reason  text,
    valid_until         date,
    version             bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at          timestamptz   NOT NULL DEFAULT now(),
    updated_at          timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX library_membership_number ON campus.library_membership (tenant_id, institution_id, number);
CREATE UNIQUE INDEX library_membership_one_live ON campus.library_membership (tenant_id, institution_id, person_kind, person_id)
    WHERE status <> 'closed';
CREATE INDEX library_membership_subject ON campus.library_membership (tenant_id, subject_ref);

CREATE TABLE campus.catalogue_record (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    title              text          NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 300),
    authors            text[]        NOT NULL DEFAULT '{}',
    isbn               text          CHECK (isbn IS NULL OR isbn ~ '^([0-9]{9}[0-9X]|[0-9]{13})$'),
    publisher          text,
    publication_year   integer       CHECK (publication_year IS NULL OR publication_year BETWEEN 1400 AND 2200),
    edition            text,
    subjects           text[]        NOT NULL DEFAULT '{}',
    resource_type      text          NOT NULL DEFAULT 'book' CHECK (resource_type IN ('book', 'journal', 'reference', 'media', 'other')),
    replacement_cost   numeric(14,2) NOT NULL DEFAULT 0 CHECK (replacement_cost >= 0),
    status             text          NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'published', 'withdrawn')),
    status_reason      text,
    version            bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX catalogue_record_isbn ON campus.catalogue_record (tenant_id, institution_id, isbn) WHERE isbn IS NOT NULL;
CREATE INDEX catalogue_record_inst ON campus.catalogue_record (tenant_id, institution_id, status);

CREATE TABLE campus.library_copy (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    record_id          uuid          NOT NULL REFERENCES campus.catalogue_record (id),
    accession_number   text          NOT NULL CHECK (accession_number ~ '^ACC-[0-9]{6,}$'),
    -- Library / reading-room space holding the copy (foundation table).
    node_id            uuid          REFERENCES campus.infrastructure_node (id),
    shelf_mark         text,
    condition          text          NOT NULL DEFAULT 'good' CHECK (condition IN ('good', 'fair', 'poor', 'damaged')),
    status             text          NOT NULL DEFAULT 'available'
        CHECK (status IN ('available', 'issued', 'reserved', 'lost', 'damaged', 'withdrawn')),
    status_reason      text,
    version            bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX library_copy_accession ON campus.library_copy (tenant_id, institution_id, accession_number);
CREATE INDEX library_copy_record ON campus.library_copy (tenant_id, record_id, status);

CREATE TABLE campus.library_loan (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    number             text          NOT NULL CHECK (number ~ '^LN-[0-9]{6,}$'),
    copy_id            uuid          NOT NULL REFERENCES campus.library_copy (id),
    record_id          uuid          NOT NULL REFERENCES campus.catalogue_record (id),
    membership_id      uuid          NOT NULL REFERENCES campus.library_membership (id),
    policy_id          uuid          NOT NULL REFERENCES campus.library_policy (id),
    person_kind        text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id          uuid          NOT NULL,
    subject_ref        text,
    issued_on          date          NOT NULL,
    due_on             date          NOT NULL,
    returned_on        date,
    renewals           integer       NOT NULL DEFAULT 0 CHECK (renewals >= 0),
    status             text          NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'overdue', 'returned', 'lost')),
    -- Last business day the overdue sweep processed this loan (idempotent per loan per day).
    last_swept_on      date,
    version            bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now(),
    CHECK (due_on >= issued_on)
);
CREATE UNIQUE INDEX library_loan_number ON campus.library_loan (tenant_id, institution_id, number);
CREATE UNIQUE INDEX library_loan_one_live_per_copy ON campus.library_loan (copy_id) WHERE status IN ('active', 'overdue');
CREATE INDEX library_loan_member ON campus.library_loan (tenant_id, membership_id, status);
CREATE INDEX library_loan_inst ON campus.library_loan (tenant_id, institution_id, status, due_on);

CREATE TABLE campus.library_reservation (
    id                 uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid          NOT NULL,
    institution_id     uuid          NOT NULL,
    record_id          uuid          NOT NULL REFERENCES campus.catalogue_record (id),
    membership_id      uuid          NOT NULL REFERENCES campus.library_membership (id),
    person_kind        text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id          uuid          NOT NULL,
    subject_ref        text,
    queue_position     integer       NOT NULL CHECK (queue_position >= 1),
    status             text          NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'ready', 'collected', 'expired', 'cancelled')),
    status_reason      text,
    copy_id            uuid          REFERENCES campus.library_copy (id),
    ready_on           date,
    hold_until         date,
    loan_id            uuid          REFERENCES campus.library_loan (id),
    version            bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz   NOT NULL DEFAULT now(),
    updated_at         timestamptz   NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX library_reservation_one_live ON campus.library_reservation (record_id, membership_id)
    WHERE status IN ('queued', 'ready');
CREATE INDEX library_reservation_queue ON campus.library_reservation (tenant_id, record_id, status, created_at);

CREATE TABLE campus.library_fine (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    loan_id              uuid          REFERENCES campus.library_loan (id),
    membership_id        uuid          NOT NULL REFERENCES campus.library_membership (id),
    person_kind          text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id            uuid          NOT NULL,
    subject_ref          text,
    kind                 text          NOT NULL CHECK (kind IN ('overdue', 'replacement')),
    days                 integer       NOT NULL DEFAULT 0 CHECK (days >= 0),
    amount               numeric(14,2) NOT NULL CHECK (amount >= 0),
    status               text          NOT NULL DEFAULT 'assessed'
        CHECK (status IN ('assessed', 'waiver_requested', 'waived', 'billed', 'paid')),
    status_reason        text,
    assessed_on          date          NOT NULL,
    process_instance_id  text,
    -- Domain F charge raised through CampusChargePort (plain uuid).
    charge_id            uuid,
    charge_number        text,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now(),
    CHECK (status NOT IN ('billed', 'paid') OR charge_id IS NOT NULL)
);
CREATE UNIQUE INDEX library_fine_one_per_loan_kind ON campus.library_fine (loan_id, kind) WHERE loan_id IS NOT NULL;
CREATE INDEX library_fine_member ON campus.library_fine (tenant_id, membership_id, status);
CREATE INDEX library_fine_inst ON campus.library_fine (tenant_id, institution_id, status);

CREATE TABLE campus.library_clearance (
    id                   uuid          PRIMARY KEY DEFAULT uuidv7(),
    tenant_id            uuid          NOT NULL,
    institution_id       uuid          NOT NULL,
    membership_id        uuid          NOT NULL REFERENCES campus.library_membership (id),
    person_kind          text          NOT NULL CHECK (person_kind IN ('student', 'staff')),
    person_id            uuid          NOT NULL,
    subject_ref          text,
    purpose              text          NOT NULL CHECK (length(btrim(purpose)) BETWEEN 1 AND 200),
    status               text          NOT NULL DEFAULT 'requested' CHECK (status IN ('requested', 'approved', 'rejected')),
    status_reason        text,
    -- Verification result at the last check: {activeLoans, unpaidFines, liveReservations, blockers[]}.
    verification         jsonb         NOT NULL DEFAULT '{}'::jsonb,
    process_instance_id  text,
    decided_by           text,
    decided_at           timestamptz,
    version              bigint        NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at           timestamptz   NOT NULL DEFAULT now(),
    updated_at           timestamptz   NOT NULL DEFAULT now()
);
CREATE INDEX library_clearance_member ON campus.library_clearance (tenant_id, membership_id, status);
CREATE INDEX library_clearance_inst ON campus.library_clearance (tenant_id, institution_id, status);

-- No physical deletion of library records (history stays reconstructible).
CREATE TRIGGER library_policy_no_delete BEFORE DELETE ON campus.library_policy
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER library_membership_no_delete BEFORE DELETE ON campus.library_membership
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER catalogue_record_no_delete BEFORE DELETE ON campus.catalogue_record
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER library_copy_no_delete BEFORE DELETE ON campus.library_copy
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER library_loan_no_delete BEFORE DELETE ON campus.library_loan
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();
CREATE TRIGGER library_fine_no_delete BEFORE DELETE ON campus.library_fine
    FOR EACH ROW EXECUTE FUNCTION campus.forbid_history_change();

