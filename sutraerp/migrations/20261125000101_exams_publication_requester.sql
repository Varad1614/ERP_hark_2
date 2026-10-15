-- Part 1 · Stage 2 (DEF-X-002): who asked for a result publication's authorisation, so the
-- authorisation decision can refuse that same user (separation of duties).
ALTER TABLE exams.publication ADD COLUMN authorisation_requested_by text;
