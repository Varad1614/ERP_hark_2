-- Part 2 · Stage 4.3 (S4-idp): identity provisioning (docs/readiness/part2/s4-idp.md).
--
-- platform.identity_link        one row per ERP subject (student / staff / admin) that has, or is
--                               to get, an identity at the IdP (Keycloak). It is both the duplicate
--                               guard (one identity per subject, one subject per IdP user, one
--                               username / e-mail per tenant) and the retryable sync queue: the
--                               desired state (`desired_state`, `roles`, `pending_actions`) is
--                               reconciled against the IdP by the `idp-sync` job, which records
--                               failures with exponential back-off (`attempts`, `next_attempt_at`).
-- platform.identity_role_request maker-checker for role changes of an identity: privileged roles
--                               (administrator, approvers) need a second user's decision.
-- platform.identity_role_mapping extra IdP targets (realm / client role, group) an ERP role maps to,
--                               per tenant and optionally per institution.
--
-- All three are tenant tables: forced row-level security via platform.apply_tenant_rls() below.

CREATE TABLE platform.identity_link (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    institution_id     uuid,
    subject_type       text        NOT NULL CHECK (subject_type IN ('student', 'staff', 'admin')),
    subject_id         uuid        NOT NULL,
    idp                text        NOT NULL DEFAULT 'keycloak' CHECK (idp IN ('keycloak', 'memory')),
    -- IdP user id (Keycloak `sub`); NULL until the first successful sync.
    idp_user_id        text        CHECK (idp_user_id IS NULL OR length(idp_user_id) BETWEEN 1 AND 200),
    username           text        NOT NULL CHECK (length(btrim(username)) BETWEEN 1 AND 255),
    email              text        CHECK (email IS NULL OR (length(email) <= 320 AND email LIKE '%_@_%')),
    given_name         text        NOT NULL CHECK (length(btrim(given_name)) BETWEEN 1 AND 200),
    family_name        text,
    -- What the ERP wants: lifecycle state of the record (`erp_state`), an administrative hold
    -- that overrides it, and the resulting `desired_state`.
    erp_state          text        NOT NULL CHECK (erp_state IN ('active', 'suspended', 'disabled')),
    admin_hold         boolean     NOT NULL DEFAULT false,
    hold_reason        text,
    desired_state      text        NOT NULL CHECK (desired_state IN ('active', 'suspended', 'disabled')),
    -- Roles the record implies (student / faculty) and roles granted through approved requests.
    base_roles         text[]      NOT NULL DEFAULT '{}',
    granted_roles      text[]      NOT NULL DEFAULT '{}',
    -- Required actions to trigger at the next sync (cleared once applied).
    pending_actions    text[]      NOT NULL DEFAULT '{}',
    send_actions_email boolean     NOT NULL DEFAULT false,
    -- Reconciliation queue.
    sync_status        text        NOT NULL DEFAULT 'pending'
        CHECK (sync_status IN ('pending', 'synced', 'failed', 'conflict')),
    attempts           integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at    timestamptz NOT NULL DEFAULT now(),
    last_error         text,
    synced_at          timestamptz,
    -- State last applied at the IdP (NULL before the first sync).
    applied_state      text        CHECK (applied_state IN ('active', 'suspended', 'disabled')),
    applied_roles      text[],
    created_by         text,
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT identity_link_subject_unique UNIQUE (tenant_id, subject_type, subject_id),
    CONSTRAINT identity_link_roles_known CHECK (
        base_roles <@ ARRAY['administrator', 'faculty', 'student', 'finance_approver',
                            'exam_controller', 'registrar', 'payroll_approver',
                            'procurement_approver']
        AND granted_roles <@ ARRAY['administrator', 'faculty', 'student', 'finance_approver',
                                   'exam_controller', 'registrar', 'payroll_approver',
                                   'procurement_approver']),
    CONSTRAINT identity_link_actions_known CHECK (
        pending_actions <@ ARRAY['VERIFY_EMAIL', 'UPDATE_PASSWORD', 'CONFIGURE_TOTP']),
    CONSTRAINT identity_link_hold_reason CHECK (NOT admin_hold OR hold_reason IS NOT NULL)
);
-- One ERP subject per IdP user — across tenants (a unique index is not filtered by RLS), so an
-- IdP account can never be adopted by a second tenant.
CREATE UNIQUE INDEX identity_link_idp_user_unique
    ON platform.identity_link (idp, idp_user_id) WHERE idp_user_id IS NOT NULL;
-- Keycloak usernames and e-mails are realm-wide.
CREATE UNIQUE INDEX identity_link_username_unique
    ON platform.identity_link (idp, lower(username));
CREATE UNIQUE INDEX identity_link_email_unique
    ON platform.identity_link (idp, lower(email)) WHERE email IS NOT NULL;
CREATE INDEX identity_link_queue_idx
    ON platform.identity_link (next_attempt_at) WHERE sync_status IN ('pending', 'failed');
CREATE INDEX identity_link_tenant_idx ON platform.identity_link (tenant_id, subject_type, sync_status);

CREATE TABLE platform.identity_role_request (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    institution_id  uuid,
    link_id         uuid        NOT NULL REFERENCES platform.identity_link (id),
    grant_roles     text[]      NOT NULL DEFAULT '{}',
    revoke_roles    text[]      NOT NULL DEFAULT '{}',
    -- Whether a second user must decide (a privileged role is granted).
    privileged      boolean     NOT NULL,
    reason          text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    status          text        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'applied')),
    requested_by    text        NOT NULL,
    decided_by      text,
    decided_at      timestamptz,
    decision_reason text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (cardinality(grant_roles) + cardinality(revoke_roles) >= 1),
    CHECK (grant_roles <@ ARRAY['administrator', 'faculty', 'student', 'finance_approver',
                                'exam_controller', 'registrar', 'payroll_approver',
                                'procurement_approver']
           AND revoke_roles <@ ARRAY['administrator', 'faculty', 'student', 'finance_approver',
                                     'exam_controller', 'registrar', 'payroll_approver',
                                     'procurement_approver']),
    CHECK ((status IN ('pending', 'applied')) OR (decided_by IS NOT NULL AND decided_at IS NOT NULL)),
    -- Separation of duties also holds in the database: the requester never decides.
    CHECK (decided_by IS NULL OR decided_by <> requested_by)
);
-- At most one open request per identity.
CREATE UNIQUE INDEX identity_role_request_one_pending
    ON platform.identity_role_request (link_id) WHERE status = 'pending';
CREATE INDEX identity_role_request_tenant_idx
    ON platform.identity_role_request (tenant_id, status, created_at);

CREATE TABLE platform.identity_role_mapping (
    id              uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id       uuid        NOT NULL,
    -- NULL = every institution of the tenant.
    institution_id  uuid,
    erp_role        text        NOT NULL CHECK (erp_role IN ('administrator', 'faculty', 'student',
                                    'finance_approver', 'exam_controller', 'registrar',
                                    'payroll_approver', 'procurement_approver')),
    target_kind     text        NOT NULL CHECK (target_kind IN ('realm_role', 'client_role', 'group')),
    -- Realm role name, client role name or group path.
    target          text        NOT NULL CHECK (length(btrim(target)) BETWEEN 1 AND 255),
    client_id       text        CHECK ((target_kind = 'client_role') = (client_id IS NOT NULL)),
    created_by      text        NOT NULL,
    created_at      timestamptz NOT NULL DEFAULT now(),
    -- A SutraERP role name is never a mapping target (it is always mapped 1:1), and SutraERP's
    -- own tenant groups are never a mapping target (they carry the tenant scope claims).
    CHECK (NOT (target_kind = 'realm_role' AND target IN ('administrator', 'faculty', 'student',
               'finance_approver', 'exam_controller', 'registrar', 'payroll_approver',
               'procurement_approver'))),
    CHECK (NOT (target_kind = 'group' AND target LIKE '/sutra-tenant-%'))
);
CREATE UNIQUE INDEX identity_role_mapping_unique
    ON platform.identity_role_mapping (tenant_id, COALESCE(institution_id, '00000000-0000-0000-0000-000000000000'::uuid),
                                       erp_role, target_kind, COALESCE(client_id, ''), target);

SELECT platform.apply_tenant_rls();
