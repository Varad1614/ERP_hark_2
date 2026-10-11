-- Batch 1D — D01/D02 self-service platform features owned by Domain A:
-- user profiles, notifications (+ read receipts, e-mail deliveries), idempotent event-consumer
-- receipts, institution-wide notices and the per-user to-do inbox.
--
-- Every table is tenant-rooted (ADR-L5-0004): each query filters by the session tenant and a
-- row of another tenant is indistinguishable from a missing one.

-- 1. Profiles -----------------------------------------------------------------------------------
-- One row per (tenant, subject), created on first sign-in / first profile read. Identity facts
-- (login id, roles) are refreshed from the authenticated principal at every sign-in; the rest is
-- self-service. `roles` lets role-addressed notifications fan out to the e-mail channel.
CREATE TABLE platform.user_profile (
    tenant_id          uuid        NOT NULL,
    subject            text        NOT NULL,
    login_id           text        NOT NULL,
    display_name       text        NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 120),
    roles              text[]      NOT NULL DEFAULT '{}',
    locale             text        NOT NULL DEFAULT 'en-IN',
    time_zone          text        NOT NULL DEFAULT 'Asia/Kolkata',
    contact_email      text,
    contact_phone      text,
    -- {"<category>": {"inApp": bool, "email": bool}}; missing entries take the defaults.
    notification_prefs jsonb       NOT NULL DEFAULT '{}'::jsonb,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    last_sign_in_at    timestamptz,
    PRIMARY KEY (tenant_id, subject)
);

CREATE INDEX user_profile_login_idx ON platform.user_profile (tenant_id, lower(login_id));
CREATE INDEX user_profile_roles_idx ON platform.user_profile USING gin (roles);

COMMENT ON TABLE platform.user_profile IS 'Owner: A Platform. Self-service profile + notification preferences per (tenant, subject).';

-- 2. Notifications --------------------------------------------------------------------------------
-- Addressed either to one subject or to every holder of a role within the tenant (there is no
-- user directory in SutraERP; Keycloak is the identity authority). Read state is per reader.
CREATE TABLE platform.notification (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id         uuid        NOT NULL,
    institution_id    uuid,
    recipient_subject text,
    recipient_role    text CHECK (recipient_role IN ('administrator', 'faculty', 'student')),
    category          text        NOT NULL CHECK (category IN ('institution', 'notice', 'todo', 'security', 'system')),
    severity          text        NOT NULL DEFAULT 'info' CHECK (severity IN ('info', 'success', 'warning', 'critical')),
    title             text        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    body              text        NOT NULL DEFAULT '' CHECK (char_length(body) <= 2000),
    -- SPA-relative link (never an absolute URL: no open redirect through notifications).
    link              text CHECK (link IS NULL OR (link LIKE '/%' AND link NOT LIKE '//%')),
    -- Event that produced it (idempotent consumer: one notification per event and recipient).
    source_event_id   uuid,
    created_at        timestamptz NOT NULL DEFAULT now(),
    -- Not shown before this instant (a scheduled notice announces itself when it goes live).
    visible_at        timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT notification_one_recipient CHECK ((recipient_subject IS NULL) <> (recipient_role IS NULL))
);

CREATE UNIQUE INDEX notification_source_recipient_idx
    ON platform.notification (source_event_id, recipient_subject, recipient_role) NULLS NOT DISTINCT
    WHERE source_event_id IS NOT NULL;

CREATE INDEX notification_subject_idx ON platform.notification (tenant_id, recipient_subject, id DESC)
    WHERE recipient_subject IS NOT NULL;
CREATE INDEX notification_role_idx ON platform.notification (tenant_id, recipient_role, id DESC)
    WHERE recipient_role IS NOT NULL;
CREATE INDEX notification_created_idx ON platform.notification (created_at);

CREATE TABLE platform.notification_read (
    notification_id uuid        NOT NULL REFERENCES platform.notification (id) ON DELETE CASCADE,
    tenant_id       uuid        NOT NULL,
    subject         text        NOT NULL,
    read_at         timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (notification_id, subject)
);

CREATE INDEX notification_read_subject_idx ON platform.notification_read (tenant_id, subject);

-- E-mail (and later SMS/push) deliveries: queued in the transaction that creates the
-- notification, sent by the advisory-locked delivery job with retry + backoff.
CREATE TABLE platform.notification_delivery (
    id                uuid        PRIMARY KEY DEFAULT uuidv7(),
    notification_id   uuid        NOT NULL REFERENCES platform.notification (id) ON DELETE CASCADE,
    tenant_id         uuid        NOT NULL,
    channel           text        NOT NULL CHECK (channel IN ('email')),
    recipient_subject text        NOT NULL,
    address           text        NOT NULL,
    status            text        NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts          integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error        text,
    next_attempt_at   timestamptz NOT NULL DEFAULT now(),
    created_at        timestamptz NOT NULL DEFAULT now(),
    sent_at           timestamptz,
    CONSTRAINT notification_delivery_once UNIQUE (notification_id, channel, recipient_subject)
);

CREATE INDEX notification_delivery_due_idx ON platform.notification_delivery (next_attempt_at)
    WHERE status = 'pending';

COMMENT ON TABLE platform.notification IS 'Owner: A Platform. In-app notifications addressed to a subject or a role (tenant scoped).';
COMMENT ON TABLE platform.notification_read IS 'Owner: A Platform. Per-reader read receipts.';
COMMENT ON TABLE platform.notification_delivery IS 'Owner: A Platform. Out-of-band delivery queue (e-mail).';

-- 3. Idempotent event-consumer receipts (ADR-L5-0010) ---------------------------------------------
-- In-process consumers read committed publication intents from platform.outbox; a receipt per
-- (consumer, event) makes processing exactly-once per consumer even though delivery is
-- at-least-once. Kept for the outbox retention window.
CREATE TABLE platform.event_receipt (
    consumer    text        NOT NULL,
    event_id    uuid        NOT NULL,
    outcome     text        NOT NULL DEFAULT 'processed' CHECK (outcome IN ('processed', 'ignored', 'failed')),
    detail      text,
    received_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (consumer, event_id)
);

CREATE INDEX event_receipt_received_idx ON platform.event_receipt (received_at);
CREATE INDEX outbox_type_created_idx ON platform.outbox (event_type, created_at);

-- 4. Notices ----------------------------------------------------------------------------------------
CREATE TABLE platform.notice (
    id             uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id      uuid        NOT NULL,
    institution_id uuid,
    title          text        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    body           text        NOT NULL CHECK (char_length(body) BETWEEN 1 AND 10000),
    audience_roles text[]      NOT NULL CHECK (cardinality(audience_roles) >= 1
                                    AND audience_roles <@ ARRAY['administrator', 'faculty', 'student']),
    pinned         boolean     NOT NULL DEFAULT false,
    publish_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz,
    status         text        NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'archived')),
    author_subject text        NOT NULL,
    author_name    text        NOT NULL,
    version        bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    archived_at    timestamptz,
    CONSTRAINT notice_window CHECK (expires_at IS NULL OR expires_at > publish_at),
    CONSTRAINT notice_archived_at CHECK ((status = 'archived') = (archived_at IS NOT NULL))
);

CREATE INDEX notice_visible_idx ON platform.notice (tenant_id, pinned DESC, publish_at DESC)
    WHERE status = 'active';
CREATE INDEX notice_audience_idx ON platform.notice USING gin (audience_roles);

COMMENT ON TABLE platform.notice IS 'Owner: A Platform. Institution-wide announcements with role audience, publish/expiry window and pin.';

-- 5. To-do inbox --------------------------------------------------------------------------------------
CREATE TABLE platform.todo (
    id               uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id        uuid        NOT NULL,
    institution_id   uuid,
    assignee_subject text        NOT NULL,
    assigned_by      text,                                   -- subject, or NULL for system/event
    title            text        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    detail           text        NOT NULL DEFAULT '' CHECK (char_length(detail) <= 4000),
    due_at           timestamptz,
    link             text CHECK (link IS NULL OR (link LIKE '/%' AND link NOT LIKE '//%')),
    source_kind      text        NOT NULL DEFAULT 'manual' CHECK (source_kind IN ('manual', 'event')),
    source_ref       text,                                   -- event id for event-created to-dos
    status           text        NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'completed', 'dismissed')),
    completed_at     timestamptz,
    dismissed_at     timestamptz,
    version          bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT todo_status_stamps CHECK (
        (status = 'completed') = (completed_at IS NOT NULL)
        AND (status = 'dismissed') = (dismissed_at IS NOT NULL))
);

CREATE INDEX todo_inbox_idx ON platform.todo (tenant_id, assignee_subject, status, id DESC);
-- An event creates at most one to-do per assignee (idempotent consumer).
CREATE UNIQUE INDEX todo_source_once_idx ON platform.todo (tenant_id, assignee_subject, source_ref)
    WHERE source_ref IS NOT NULL;
CREATE INDEX todo_closed_idx ON platform.todo (updated_at) WHERE status <> 'open';

COMMENT ON TABLE platform.todo IS 'Owner: A Platform. Per-user actionable items (manual or created from domain events).';
