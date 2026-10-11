-- Batch 1D — dev-mode WebAuthn second factor and OIDC back-channel logout (ADR-L5-0038 §3.9,
-- ADR-L5-0024).

-- 1. WebAuthn credentials ----------------------------------------------------------------------
-- Public-key credentials registered through the webauthn-rs relying party (dev auth mode).
-- `passkey` is the serialized webauthn-rs credential (public key, credential id, flags) — public
-- material, no secret. `sign_count` mirrors the authenticator counter so a cloned authenticator
-- (non-increasing counter) is refused even when two assertions race.
CREATE TABLE platform.webauthn_credential (
    id            uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id     uuid        NOT NULL,
    subject       text        NOT NULL,
    credential_id bytea       NOT NULL CHECK (octet_length(credential_id) BETWEEN 16 AND 1023),
    label         text        NOT NULL CHECK (char_length(label) BETWEEN 1 AND 64),
    passkey       jsonb       NOT NULL,
    sign_count    bigint      NOT NULL DEFAULT 0 CHECK (sign_count >= 0),
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz,
    CONSTRAINT webauthn_credential_id_key UNIQUE (credential_id)
);

CREATE INDEX webauthn_credential_subject_idx ON platform.webauthn_credential (tenant_id, subject);

COMMENT ON TABLE platform.webauthn_credential IS 'Owner: A Platform (D01). Dev-mode WebAuthn credentials + sign counters.';

-- 2. Pre-auth records: WebAuthn ceremony state ---------------------------------------------------
ALTER TABLE platform.pre_auth DROP CONSTRAINT pre_auth_kind_check;
ALTER TABLE platform.pre_auth ADD CONSTRAINT pre_auth_kind_check
    CHECK (kind IN ('oidc', 'mfa', 'webauthn_reg', 'webauthn_auth'));

-- 3. Replay guard ------------------------------------------------------------------------------------
-- Single-use identifiers (OIDC back-channel logout token `jti`) remembered until they could no
-- longer be replayed. Only SHA-256(namespace | key) is stored.
CREATE TABLE platform.replay_guard (
    key_hash   bytea       PRIMARY KEY CHECK (octet_length(key_hash) = 32),
    namespace  text        NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);

CREATE INDEX replay_guard_expires_idx ON platform.replay_guard (expires_at);

COMMENT ON TABLE platform.replay_guard IS 'Owner: A Platform (D01). Seen single-use identifiers (logout-token jti).';
