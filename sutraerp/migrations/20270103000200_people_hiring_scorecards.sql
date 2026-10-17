-- ERP_EXTENSIONS 3B — hiring scorecards. Logic ported from HeyPuter/everything hiring
-- src/core/scorecards.js, guides.js, screening.js, reminders.js (MIT); see THIRD_PARTY_NOTICES.md.
--
-- Interviewers rate a candidate against a rubric (the requisition's own, the institution's, or
-- the default four) and give a recommendation; a scorecard snapshots the rubric and the guide
-- questions it answered so later edits never rewrite feedback. Interview guides attach to an
-- interview format or a candidate stage. Screening questions sit on the requisition; the
-- candidate's answers are stored with the question text.
CREATE TABLE people.hiring_settings (
    tenant_id          uuid         NOT NULL,
    institution_id     uuid         NOT NULL,
    rating_fields      jsonb        CHECK (rating_fields IS NULL OR jsonb_typeof(rating_fields) = 'array'),
    guides             jsonb        NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(guides) = 'array'),
    version            bigint       NOT NULL DEFAULT 1,
    updated_at         timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, institution_id)
);

ALTER TABLE people.requisition
    ADD COLUMN rating_fields       jsonb CHECK (rating_fields IS NULL OR jsonb_typeof(rating_fields) = 'array'),
    ADD COLUMN screening_questions jsonb CHECK (screening_questions IS NULL OR jsonb_typeof(screening_questions) = 'array');
ALTER TABLE people.candidate
    ADD COLUMN screening_answers   jsonb CHECK (screening_answers IS NULL OR jsonb_typeof(screening_answers) = 'array');

CREATE TABLE people.candidate_interview (
    id                 uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid         NOT NULL,
    institution_id     uuid         NOT NULL,
    candidate_id       uuid         NOT NULL REFERENCES people.candidate (id),
    interview_type     text         NOT NULL CHECK (interview_type IN ('phone', 'video', 'onsite')),
    interview_date     date         NOT NULL,
    start_time         text         NOT NULL CHECK (start_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    end_time           text         NOT NULL CHECK (end_time ~ '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
    interviewers       text[]       NOT NULL DEFAULT '{}',
    location           text         CHECK (location IS NULL OR length(location) <= 200),
    guide_id           text,
    status             text         NOT NULL DEFAULT 'scheduled' CHECK (status IN ('scheduled', 'cancelled')),
    version            bigint       NOT NULL DEFAULT 1,
    created_at         timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX candidate_interview_candidate_idx ON people.candidate_interview (tenant_id, candidate_id);
CREATE INDEX candidate_interview_date_idx ON people.candidate_interview (tenant_id, institution_id, interview_date);

CREATE TABLE people.candidate_scorecard (
    id                 uuid         PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid         NOT NULL,
    institution_id     uuid         NOT NULL,
    candidate_id       uuid         NOT NULL REFERENCES people.candidate (id),
    interview_id       uuid         REFERENCES people.candidate_interview (id),
    author_name        text         NOT NULL CHECK (length(btrim(author_name)) BETWEEN 1 AND 120),
    author_subject     text,
    recommendation     text         NOT NULL CHECK (recommendation IN ('strong_yes', 'yes', 'no', 'strong_no')),
    ratings            jsonb        NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(ratings) = 'object'),
    fields             jsonb        NOT NULL CHECK (jsonb_typeof(fields) = 'array'),
    guide_answers      jsonb        CHECK (guide_answers IS NULL OR jsonb_typeof(guide_answers) = 'array'),
    notes              text         CHECK (notes IS NULL OR length(notes) <= 4000),
    created_on         date         NOT NULL,
    created_at         timestamptz  NOT NULL DEFAULT now()
);
CREATE INDEX candidate_scorecard_candidate_idx ON people.candidate_scorecard (tenant_id, candidate_id);

GRANT SELECT, INSERT, UPDATE, DELETE ON people.hiring_settings, people.candidate_interview,
    people.candidate_scorecard TO sutra_people;
SELECT platform.apply_tenant_rls();
