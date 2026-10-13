//! OpenAPI document root. Paths/schemas are collected from the module routers (utoipa-axum).

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "SutraERP API",
        version = "1.0.0",
        description = "SutraERP education ERP — synchronous REST/JSON API (major version v1).\n\n\
            Conventions (ADR-L5-0034): camelCase JSON members; URL major versioning (`/api/v1`); \
            RFC 9457 `application/problem+json` errors with `errorCode`, `correlationId` and \
            `validationErrors` extensions; retry-sensitive commands accept `Idempotency-Key`.\n\n\
            Authentication (ADR-L5-0024/0038): dev mode signs in with `POST /api/v1/auth/login` \
            (+ TOTP `POST /api/v1/auth/mfa/verify`); Keycloak mode with the OIDC Authorization \
            Code + PKCE flow `GET /api/v1/auth/oidc/start` → `/oidc/callback`. Both set an opaque \
            `HttpOnly` session cookie; the session's `csrfToken` must be sent as `X-CSRF-Token` \
            on every POST/PUT/PATCH/DELETE made with the cookie. Login and MFA verification \
            instead need the signed double-submit token from `GET /api/v1/auth/csrf`. Unsafe \
            requests from another site (`Origin` / `Sec-Fetch-Site`) are rejected. \
            Tenant/institution scope is taken from the session (ADR-L5-0004); scope headers \
            cannot select or widen it. Admin-sensitive permissions need an MFA-verified session \
            (`mfa-step-up-required`). Missing session: 401; CSRF failure, cross-site request, \
            missing permission or step-up: 403 (`application/problem+json`)."
    ),
    tags(
        (name = "health", description = "Liveness and readiness probes"),
        (name = "auth", description = "D01 authentication boundary (Keycloak-delegated, server-side session)"),
        (name = "platform", description = "A Platform: catalog and module registry"),
        (name = "institution", description = "B Institution: institution aggregate, lifecycle, profile, timezone and policies"),
        (name = "organisation", description = "B Institution: organisation units, type blueprints, governance and position definitions"),
        (name = "campuses", description = "B Institution: institutional campuses"),
        (name = "curriculum", description = "B Institution: academic structure, programme catalogue, curriculum framework and school academic model"),
        (name = "calendar", description = "B Institution: academic calendar, years, terms, assignments and exceptions"),
        (name = "readiness", description = "B Institution: activation readiness checklist"),
        (name = "admissions", description = "C Student lifecycle: admission applications, offers, document verification, approval, financial clearance, confirmation, cancellation, withdrawal, revocation, expiry and readmission"),
        (name = "students", description = "C Student lifecycle: student record and StudentContext, enrolment, academic assignment, suspension, transfer, profile, identity, duplicates, documents and certificate requests, archival, Student 360"),
        (name = "registration", description = "C Student lifecycle: registration windows, academic registration, add/drop, cancellation, backlog/repeat, finalisation and holds"),
        (name = "progression", description = "C Student lifecycle: progression decisions and policy, credit ledger, probation, detention, credit transfer, programme migration, academic break, qualification eligibility and ABC sync"),
        (name = "completion", description = "C Student lifecycle: qualification completion and conferment, academic record finalisation, student exits and exit clearance, deferral, correction and revocation"),
        (name = "guardians", description = "C Student lifecycle: guardian relationships, custody, consent, communication authorisation, emergency contacts, financial responsibility and pickup authorisation"),
        (name = "scholarships", description = "C Student lifecycle: scholarship definitions and eligibility policy, eligibility determination, renewal, recalculation and financial-aid hand-off"),
        (name = "alumni", description = "C Student lifecycle: student-to-alumni transition, correction and archival"),
        (name = "offerings", description = "D Academic delivery: course offerings, sections, capacity and quotas, enrolment windows, eligibility, waitlist, revisions, merge/split, freeze, templates and change simulation"),
        (name = "faculty-assignments", description = "D Academic delivery: faculty academic assignment, operational change, governance configuration and faculty My classes"),
        (name = "timetable", description = "D Academic delivery: scheduling requirements, timetable generation, validation, publication, revision and retirement; student and faculty timetables"),
        (name = "attendance", description = "D Academic delivery: teaching sessions, attendance capture, finalisation, correction and thresholds, syllabus coverage"),
        (name = "monitoring", description = "D Academic delivery: academic monitoring, risk assessment, exceptions, interventions, review and closure"),
        (name = "assessment", description = "D Academic delivery: internal assessment blueprints, assessment windows and coursework assignments, submissions, moderation, finalisation and correction"),
        (name = "flexible-learning", description = "D Academic delivery: learning providers, flexible and external learning opportunities, demand, seat allocation, waitlist, external registration and completion"),
        (name = "credit-recognition", description = "D Academic delivery: external certificate verification, credit mapping rules, credit recognition and posting, reconciliation"),
        (name = "resources", description = "D Academic delivery: course resources and library links"),
        (name = "finance-accounts", description = "F Finance: student / applicant finance accounts and balances (foundation)"),
        (name = "finance-charges", description = "F Finance: fee heads, fee structures, student charges, revision, cancellation / suspension, disputes, instalments and settlement closure"),
        (name = "finance-adjustments", description = "F Finance: credit notes, debit notes, write-offs and waivers, reversal and corrected successors"),
        (name = "financial-aid", description = "F Finance: financial aid allocation from approved scholarship decisions, disbursement, renewal, revocation and recovery, programme reconciliation"),
        (name = "finance-config", description = "F Finance: governed finance configuration and financial rules"),
        (name = "finance-payments", description = "F Finance: payments, gateway orders and webhooks, allocation, receipts, unidentified receipts, failure recovery, reversal, bulk collection"),
        (name = "finance-refunds", description = "F Finance: refund requests, approval, completion, cancellation, failure and reversal"),
        (name = "finance-receivables", description = "F Finance: accounts receivable, dunning, settlement, write-off and reopening"),
        (name = "finance-ledger", description = "F Finance: chart of accounts, journals, manual journals, accounting batches, reversal, reconciliation, trial balance and accounting close"),
        (name = "finance-periods", description = "F Finance: financial years and periods (open, lock, close, reopen, archive)"),
        (name = "finance-treasury", description = "F Finance: bank accounts, bank execution, statements and reconciliation, cash position, fund transfers, failed bank transactions"),
        (name = "notifications", description = "A Platform: in-app notifications (subject or role addressed, per-reader read state)"),
        (name = "notices", description = "A Platform: institution-wide notices with role audience and publish/expiry window"),
        (name = "todos", description = "A Platform: per-user to-do inbox"),
        (name = "profile", description = "A Platform: self-service profile and notification preferences")
    ),
    components(schemas(sutra_web::Problem)),
    modifiers(&SecuritySchemes)
)]
pub struct ApiDoc;

/// Declares the session cookie and CSRF header security schemes referenced by protected paths.
struct SecuritySchemes;

impl utoipa::Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "__Host-sutra_session",
                "Opaque server-side session token (`sutra_session` when Secure cookies are off in dev).",
            ))),
        );
        components.add_security_scheme(
            "login_csrf",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "Pre-session double-submit token from GET /api/v1/auth/csrf (must equal the \
                 `__Host-sutra_login_csrf` cookie).",
            ))),
        );
        components.add_security_scheme(
            "csrf_token",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "Session-bound synchronizer token from login or GET /api/v1/auth/session.",
            ))),
        );
    }
}

/// Pretty JSON of the full generated specification (used by `sutra-api openapi` and CI drift check).
pub fn spec_json() -> Result<String, serde_json::Error> {
    let (_, api) = crate::app::build(crate::app::AppContext::for_spec()).split_for_parts();
    let mut s = serde_json::to_string_pretty(&api)?;
    s.push('\n');
    Ok(s)
}
