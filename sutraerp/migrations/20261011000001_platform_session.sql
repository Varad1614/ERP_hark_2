-- D01 durable server-side sessions (ADR-L5-0024 / ADR-L5-0038 §3.4, ADR-L5-0037 §2.8).
--
-- * The opaque session token never reaches the database: only SHA-256(token) is stored and used
--   as the lookup key, so a dump of this table cannot be replayed as a cookie.
-- * csrf_secret is a per-session random key; the synchronizer token given to the browser is
--   HMAC-SHA256(csrf_secret, token) and therefore also cannot be derived from this table alone.
-- * A session is live while revoked_at IS NULL, now() < expires_at (absolute) and
--   now() < last_seen_at + idle timeout (sliding; the idle timeout is configuration).
-- * Valkey may cache sessions later but is never the authority.
CREATE TABLE platform.session (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),          -- technical id, safe to log
    token_hash      bytea       NOT NULL CHECK (octet_length(token_hash) = 32),
    csrf_secret     bytea       NOT NULL CHECK (octet_length(csrf_secret) = 32),
    -- Principal snapshot taken at login (Keycloak `sub` in production).
    subject         text        NOT NULL,
    login_id        text        NOT NULL,
    display_name    text        NOT NULL,
    roles           text[]      NOT NULL DEFAULT '{}',
    -- Session-bound execution scope (ADR-L5-0004): the scope authority for protected calls.
    tenant_id       uuid        NOT NULL,
    institution_id  uuid,
    created_at      timestamptz NOT NULL DEFAULT now(),
    last_seen_at    timestamptz NOT NULL DEFAULT now(),
    expires_at      timestamptz NOT NULL,
    revoked_at      timestamptz,
    CONSTRAINT session_token_hash_key UNIQUE (token_hash),
    CONSTRAINT session_expiry_after_creation CHECK (expires_at > created_at)
);

-- "Sign out everywhere" / per-subject administration.
CREATE INDEX session_subject_live_idx ON platform.session (tenant_id, subject) WHERE revoked_at IS NULL;
-- Expiry sweeper (scheduled in a later Batch 1 slice).
CREATE INDEX session_expires_at_idx ON platform.session (expires_at);

COMMENT ON TABLE platform.session IS 'Owner: A Platform (D01). Server-side sessions; token stored as SHA-256 only.';
COMMENT ON COLUMN platform.session.token_hash IS 'SHA-256 of the opaque session token. The token itself is never stored.';
COMMENT ON COLUMN platform.session.csrf_secret IS 'Per-session key; CSRF token = HMAC-SHA256(csrf_secret, token).';
