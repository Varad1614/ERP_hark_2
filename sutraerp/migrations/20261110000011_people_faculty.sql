-- Batch 7 slice 7A — faculty profile and teaching eligibility (L2-E03; L3-E-026, 029, 031).

CREATE TABLE people.faculty_profile (
    id                        uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id                 uuid         NOT NULL,
    institution_id            uuid         NOT NULL,
    staff_id                  uuid         NOT NULL REFERENCES people.staff_member (id),
    engagement_id             uuid         NOT NULL REFERENCES people.staff_engagement (id),
    disciplines               text         CHECK (disciplines IS NULL OR length(btrim(disciplines)) BETWEEN 1 AND 300),
    highest_qualification     text         CHECK (highest_qualification IS NULL OR length(btrim(highest_qualification)) BETWEEN 1 AND 200),
    -- Faculty workforce status (L3-E-031): active | suspended.
    status                    text         NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'suspended')),
    status_reason             text,
    -- Teaching eligibility (L3-E-029); NULL until first requested.
    eligibility_status        text         CHECK (eligibility_status IS NULL OR eligibility_status IN
                                               ('requested', 'under_review', 'approved', 'suspended', 'expired', 'rejected')),
    eligibility_valid_until   date,
    eligibility_reason        text,
    version                   bigint       NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at                timestamptz  NOT NULL DEFAULT now(),
    updated_at                timestamptz  NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX faculty_profile_one_per_staff ON people.faculty_profile (staff_id);
CREATE INDEX faculty_profile_institution_idx ON people.faculty_profile (tenant_id, institution_id, status);
CREATE TRIGGER faculty_profile_no_delete BEFORE DELETE ON people.faculty_profile
    FOR EACH ROW EXECUTE FUNCTION people.forbid_history_change();
