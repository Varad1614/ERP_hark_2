-- Part 2 · Stage 7 (S7-entitle) — module entitlements and licence state
-- (docs/readiness/ENTITLEMENT_IMPLEMENTATION.md; registry: catalog/entitlements.json).
--
-- * platform.tenant_entitlement          what one tenant (institution group) has activated: a
--                                        package, add-on modules, the licence status
--                                        (active / grace / expired / suspended), its validity
--                                        window and grace days. One row per tenant. A tenant
--                                        without a row gets SUTRA_ENTITLEMENT_DEFAULT (default
--                                        complete_erp, perpetual) so every existing tenant, the
--                                        dev seed and the demo keep every module.
-- * platform.tenant_entitlement_request  maker-checker for every change: a platform administrator
--                                        requests the full desired state, a second, independent
--                                        holder of platform.entitlement.approve decides it; an
--                                        approved request is applied in the same transaction.
--                                        Separation of duties also holds in the database
--                                        (decided_by <> requested_by).
--
-- Module keys are validated against the registry by the API (dependency closure included); the
-- CHECKs below pin the vocabulary so no other writer can store an unknown key or status.
-- Both are tenant tables: forced row-level security via platform.apply_tenant_rls() below.

CREATE TABLE platform.tenant_entitlement (
    tenant_id          uuid        PRIMARY KEY,
    package            text        CHECK (package IN ('core_academic', 'academic_plus',
                                       'institution_operations', 'complete_erp')),
    add_on_modules     text[]      NOT NULL DEFAULT '{}'
        CHECK (add_on_modules <@ ARRAY['students', 'admissions', 'academics', 'attendance',
               'examinations', 'fees', 'hr', 'payroll', 'hostel', 'transport', 'library',
               'procurement', 'inventory', 'assets', 'reporting']),
    licence_status     text        NOT NULL DEFAULT 'active'
        CHECK (licence_status IN ('active', 'grace', 'expired', 'suspended')),
    valid_from         timestamptz NOT NULL,
    -- NULL = perpetual.
    valid_until        timestamptz,
    grace_days         integer     NOT NULL DEFAULT 0 CHECK (grace_days BETWEEN 0 AND 365),
    version            bigint      NOT NULL DEFAULT 1 CHECK (version >= 1),
    -- The approved request this state came from.
    request_id         uuid        NOT NULL,
    updated_by         text        NOT NULL,
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CHECK (valid_until IS NULL OR valid_until > valid_from)
);

CREATE TABLE platform.tenant_entitlement_request (
    id                 uuid        PRIMARY KEY DEFAULT uuidv7(),
    tenant_id          uuid        NOT NULL,
    -- What the request does (for the audit trail and the approver's screen).
    kind               text        NOT NULL CHECK (kind IN ('activate', 'change', 'renew',
                                       'suspend', 'reinstate')),
    package            text        CHECK (package IN ('core_academic', 'academic_plus',
                                       'institution_operations', 'complete_erp')),
    add_on_modules     text[]      NOT NULL DEFAULT '{}'
        CHECK (add_on_modules <@ ARRAY['students', 'admissions', 'academics', 'attendance',
               'examinations', 'fees', 'hr', 'payroll', 'hostel', 'transport', 'library',
               'procurement', 'inventory', 'assets', 'reporting']),
    licence_status     text        NOT NULL
        CHECK (licence_status IN ('active', 'grace', 'expired', 'suspended')),
    valid_from         timestamptz NOT NULL,
    valid_until        timestamptz,
    grace_days         integer     NOT NULL CHECK (grace_days BETWEEN 0 AND 365),
    -- Version of platform.tenant_entitlement the request was made against (0 = no row yet);
    -- a decision on a stale request is refused.
    base_version       bigint      NOT NULL DEFAULT 0 CHECK (base_version >= 0),
    reason             text        NOT NULL CHECK (length(btrim(reason)) BETWEEN 1 AND 1000),
    status             text        NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'withdrawn')),
    requested_by       text        NOT NULL,
    requested_at       timestamptz NOT NULL DEFAULT now(),
    decided_by         text,
    decided_at         timestamptz,
    decision_reason    text,
    CHECK (valid_until IS NULL OR valid_until > valid_from),
    CHECK ((status = 'pending') OR (decided_by IS NOT NULL AND decided_at IS NOT NULL)),
    -- Separation of duties: the requester never decides (withdrawal is by the requester).
    CHECK (status = 'withdrawn' OR decided_by IS NULL OR decided_by <> requested_by)
);
-- At most one open request per tenant.
CREATE UNIQUE INDEX tenant_entitlement_request_one_pending
    ON platform.tenant_entitlement_request (tenant_id) WHERE status = 'pending';
CREATE INDEX tenant_entitlement_request_tenant_idx
    ON platform.tenant_entitlement_request (tenant_id, requested_at DESC);

COMMENT ON TABLE platform.tenant_entitlement IS
    'Part 2 S7-entitle: activated package / add-on modules and licence state of a tenant (registry: catalog/entitlements.json).';
COMMENT ON TABLE platform.tenant_entitlement_request IS
    'Part 2 S7-entitle: maker-checker change requests for platform.tenant_entitlement (requester never decides).';

SELECT platform.apply_tenant_rls();
