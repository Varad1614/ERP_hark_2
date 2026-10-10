-- Batch 1B — D01 authentication assurance, MFA factors and pre-auth records
-- (ADR-L5-0024 Keycloak OIDC, ADR-L5-0038 §3.4/§3.8 sessions + MFA).

-- 1. Session assurance + IdP session id ---------------------------------------------------------
ALTER TABLE platform.session
    ADD COLUMN mfa_level text   NOT NULL DEFAULT 'none' CHECK (mfa_level IN ('none', 'mfa')),
    ADD COLUMN amr       text[] NOT NULL DEFAULT '{}',
    ADD COLUMN idp_sid   text;

-- RP-initiated / back-channel logout looks sessions up by the Keycloak `sid`.
CREATE INDEX session_idp_sid_idx ON platform.session (idp_sid) WHERE idp_sid IS NOT NULL;

COMMENT ON COLUMN platform.session.mfa_level IS 'none | mfa — second factor verified for this session (step-up gate).';
COMMENT ON COLUMN platform.session.amr IS 'Authentication methods references (pwd, otp, hwk, mfa …).';
COMMENT ON COLUMN platform.session.idp_sid IS 'Keycloak session id (sid claim) for federated sessions.';

-- 2. MFA factors ---------------------------------------------------------------------------------
-- Dev-mode TOTP (Keycloak mode keeps factors in Keycloak). The shared secret is stored only as
-- AES-256-GCM ciphertext (nonce ‖ ciphertext ‖ tag) with tenant + subject + kind as associated
-- data, so a row copied to another subject fails to decrypt. key_version allows key rotation.
CREATE TABLE platform.mfa_factor (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    subject           text        NOT NULL,
    kind              text        NOT NULL CHECK (kind IN ('totp', 'webauthn')),
    status            text        NOT NULL CHECK (status IN ('pending', 'active')),
    secret_ciphertext bytea       NOT NULL CHECK (octet_length(secret_ciphertext) > 28),
    key_version       smallint    NOT NULL DEFAULT 1,
    -- Last accepted TOTP time step: codes for this or an earlier step are replays.
    last_used_step    bigint,
    created_at        timestamptz NOT NULL DEFAULT now(),
    confirmed_at      timestamptz,
    CONSTRAINT mfa_factor_active_confirmed CHECK (status <> 'active' OR confirmed_at IS NOT NULL)
);

-- At most one active and one pending factor of each kind per subject.
CREATE UNIQUE INDEX mfa_factor_one_per_status_idx
    ON platform.mfa_factor (tenant_id, subject, kind, status);

COMMENT ON TABLE platform.mfa_factor IS 'Owner: A Platform (D01). MFA factors; secrets AES-256-GCM encrypted at rest.';
COMMENT ON COLUMN platform.mfa_factor.secret_ciphertext IS 'nonce(12) ‖ AES-256-GCM ciphertext ‖ tag(16); AAD = tenant|subject|kind.';

-- 3. Pre-authentication records ------------------------------------------------------------------
-- Short-lived, single-use server-side state for flows that have no session yet: the OIDC
-- authorization request (state → nonce, PKCE verifier, browser binding, return path) and the
-- pending MFA challenge. Only SHA-256(handle) is stored.
CREATE TABLE platform.pre_auth (
    handle_hash bytea       PRIMARY KEY CHECK (octet_length(handle_hash) = 32),
    kind        text        NOT NULL CHECK (kind IN ('oidc', 'mfa')),
    payload     jsonb       NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    CONSTRAINT pre_auth_expiry_after_creation CHECK (expires_at > created_at)
);

CREATE INDEX pre_auth_expires_at_idx ON platform.pre_auth (expires_at);

COMMENT ON TABLE platform.pre_auth IS 'Owner: A Platform (D01). Single-use pre-auth records (OIDC state, MFA challenges); handle stored as SHA-256 only.';
