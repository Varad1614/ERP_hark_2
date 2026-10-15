//! Slice 9E DB tests — campus governance (G00) and accreditation (H09), Batch 9.
//!
//! * `governance_g001_to_g015_through_the_api` — every G00 scenario through the HTTP API:
//!   versioned configuration (G-014, supersession, the effective policy feeding the closure
//!   rules), readiness / health / maturity assessments (G-001 / 008 / 010), operational and
//!   executive reviews (G-003 / 004), the risk register with verified mitigation and the risk
//!   appetite (G-005), a multi-domain action plan with verified closure (G-006), an SLA deviation
//!   over 8A service tickets (G-007), a KPI review over the campus figures (G-009), a finding →
//!   corrective action (G-011), an escalation → executive resolution (G-012), an exception
//!   (G-013) and governance evidence published for accreditation (G-002 / G-015), plus ETag /
//!   If-Match, idempotency headers, permissions and audit rows.
//! * `governance_await_decision` — `ApprovalMode::AwaitDecision` through
//!   `sutra_campus::router_with_state`: pending case, rejection (reason required) back to open,
//!   approval by `campus.approve`, stale ETags refused, configuration awaiting its decision.
//! * `accreditation_h061_to_h071_through_the_api` — framework versions (H-061), cycle +
//!   approved assessment plan (H-062), obligations (H-063), evidence citing a validated 8C
//!   regulatory snapshot and published governance evidence (H-064), internal audit → finding →
//!   verified CAPA (H-065 / 067), external assessment (H-066), quality improvement measured
//!   through the metrics port (H-068), readiness decision (H-069), periodic compliance review
//!   (H-070) and the self-study report approval + hand-off (H-071); plus the §6.4 approval-state
//!   wire field on an 8D supplier.
//! * `accreditation_await_decision` — `ApprovalMode::AwaitDecision` through
//!   `sutra_services::router_with_state`.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use support::b4;
use support::b8;
use support::*;
use sutra_campus::shared::approval::ApprovalMode;
use sutra_contracts::campus::status::{SpaceType, TicketCategory, TicketSource};
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b9e00";
const TENANT_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b9e01";
const TENANT_ACCR: &str = "0192f5a0-7c3e-7000-8000-0000000b9e02";
const TENANT_ACCR_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b9e03";
const C: &str = "/api/v1/campus";
const S: &str = "/api/v1/services";
const R: &str = "/api/v1/resources";
const ADMIN: &str = "dev|admin@sutra.dev";

async fn call(
    app: &axum::Router,
    who: &Login,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "POST", path, body, if_match)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

/// POST that must answer `want`; returns the body.
async fn ok(
    app: &axum::Router,
    who: &Login,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, b) = call(app, who, path, body, if_match).await;
    assert_eq!(s, want, "POST {path}: {b}");
    b
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn sid(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

/// The `n`-th item of a case / record.
fn item(v: &Value, n: usize) -> (String, i64) {
    let i = &v["items"][n];
    (
        i["id"].as_str().unwrap().to_owned(),
        i["version"].as_i64().unwrap(),
    )
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn today() -> NaiveDate {
    (Utc::now() + Duration::minutes(330)).date_naive()
}

async fn outbox(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Opens a case and starts it (draft → open).
async fn open_case(app: &axum::Router, who: &Login, inst: Uuid, body: Value) -> Value {
    let c = ok(
        app,
        who,
        &format!("{C}/institutions/{inst}/governance-cases"),
        body,
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(c["status"], "draft", "{c}");
    assert!(c["number"].as_str().unwrap().starts_with("GOV-"));
    ok(
        app,
        who,
        &format!("{C}/governance-cases/{}/start", sid(&c)),
        json!({}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await
}

async fn add_item(app: &axum::Router, who: &Login, case: &Value, body: Value) -> Value {
    ok(
        app,
        who,
        &format!("{C}/governance-cases/{}/items", sid(case)),
        body,
        None,
        StatusCode::CREATED,
    )
    .await
}

/// Moves item `n` of the case to `status` (returns the case).
async fn progress(app: &axum::Router, who: &Login, case: &Value, n: usize, body: Value) -> Value {
    let (id, v) = item(case, n);
    ok(
        app,
        who,
        &format!("{C}/governance-items/{id}/progress"),
        body,
        Some(v),
        StatusCode::OK,
    )
    .await
}

async fn verify(app: &axum::Router, who: &Login, case: &Value, n: usize) -> Value {
    let (id, v) = item(case, n);
    ok(
        app,
        who,
        &format!("{C}/governance-items/{id}/verify"),
        json!({"evidenceRef": "DMS-2026-0042"}),
        Some(v),
        StatusCode::OK,
    )
    .await
}

async fn submit(app: &axum::Router, who: &Login, case: &Value, body: Value) -> (StatusCode, Value) {
    call(
        app,
        who,
        &format!("{C}/governance-cases/{}/submit", sid(case)),
        body,
        Some(ver(case)),
    )
    .await
}

/// Submits and expects auto-approval to close the case with `outcome`.
async fn close(app: &axum::Router, who: &Login, case: &Value, body: Value) -> Value {
    let (s, c) = submit(app, who, case, body).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        (c["status"].as_str(), c["approvalState"].as_str()),
        (Some("closed"), Some("approved")),
        "{c}"
    );
    c
}

async fn reread(app: &axum::Router, who: &Login, case: &Value) -> Value {
    let (s, c) = read(app, who, &format!("{C}/governance-cases/{}", sid(case))).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    c
}

// ==================================================================== G00 through the API

#[tokio::test]
async fn governance_g001_to_g015_through_the_api() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera_login = login(&app, "meera.shah@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT, "Governance University").await;
    let inst = setup.institution().as_uuid();
    b8::seed_staff(&pool, &setup, "Meera", Some(b4::MEERA)).await;
    let cases = format!("{C}/institutions/{inst}/governance-cases");

    // ---- permissions: faculty neither read nor write governance.
    let (s, _) = call(
        &app,
        &meera_login,
        &cases,
        json!({"kind": "risk", "title": "x", "likelihood": 2, "impact": 2}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = read(&app, &meera_login, &cases).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ================================================================ G-014 configuration
    let cfgs = format!("{C}/institutions/{inst}/governance-configs");
    let (s, bad) = call(
        &app,
        &admin,
        &cfgs,
        json!({"key": "risk_appetite", "settings": {"maxResidualScore": 40}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    let c1 = ok(
        &app,
        &admin,
        &cfgs,
        json!({"key": "risk_appetite", "settings": {"maxResidualScore": 10}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (
            c1["status"].as_str(),
            c1["approvalState"].as_str(),
            c1["versionNo"].as_i64()
        ),
        (Some("draft"), Some("not_requested"), Some(1))
    );
    let c1 = ok(
        &app,
        &admin,
        &format!("{C}/governance-configs/{}/submit", sid(&c1)),
        json!({}),
        Some(ver(&c1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (c1["status"].as_str(), c1["approvalState"].as_str()),
        (Some("effective"), Some("approved")),
        "{c1}"
    );
    let c2 = ok(
        &app,
        &admin,
        &cfgs,
        json!({"key": "risk_appetite", "settings": {"maxResidualScore": 8}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(c2["versionNo"], 2);
    let c2 = ok(
        &app,
        &admin,
        &format!("{C}/governance-configs/{}/submit", sid(&c2)),
        json!({}),
        Some(ver(&c2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c2["status"], "effective");
    assert_eq!(c2["predecessorId"], c1["id"]);
    let (_, old) = read(
        &app,
        &admin,
        &format!("{C}/governance-configs/{}", sid(&c1)),
    )
    .await;
    assert_eq!(old["status"], "superseded");
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-configs/{}/submit", sid(&c2)),
        json!({}),
        Some(ver(&c2)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "an effective version is not resubmitted"
    );
    let (s, pol) = read(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/governance-policy"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pol}");
    assert_eq!(pol["policy"]["maxResidualScore"], 8);
    assert_eq!(pol["policy"]["minCriterionScore"], 3, "default");
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.campus.governance_config.effective",
            &sid(&c2)
        )
        .await,
        1
    );

    // ================================================================ G-001 readiness assessment
    let ra = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "readiness_assessment", "title": "Semester start readiness",
            "domains": ["facility", "hostel"]}),
    )
    .await;
    assert_eq!(ra["status"], "open");
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/items", sid(&ra)),
        json!({"kind": "recommendation", "title": "nope"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "assessments take criteria"
    );
    let ra = add_item(
        &app,
        &admin,
        &ra,
        json!({"kind": "criterion", "title": "Classrooms ready", "score": 4}),
    )
    .await;
    let ra = add_item(
        &app,
        &admin,
        &ra,
        json!({"kind": "criterion", "title": "Hostel rooms ready", "score": 2}),
    )
    .await;
    let (s, e) = submit(&app, &admin, &ra, json!({"outcome": "ready"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a criterion is below 3: {e}"
    );
    let (s, _) = submit(&app, &admin, &ra, json!({"outcome": "maybe"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/submit", sid(&ra)),
        json!({"outcome": "ready"}),
        Some(ver(&ra) + 7),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale ETag");
    let (s, _, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("{C}/governance-cases/{}/submit", sid(&ra)))
            .header("content-type", "application/json")
            .header("cookie", &admin.cookie)
            .header("x-csrf-token", &admin.csrf)
            .body(Body::from(json!({"outcome": "ready"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED, "If-Match is required");
    let ra = progress(&app, &admin, &ra, 1, json!({"score": 3})).await;
    assert_eq!(ra["averageScore"], 3.5);
    let ra = close(
        &app,
        &admin,
        &ra,
        json!({"outcome": "ready", "rationale": "All blocks inspected"}),
    )
    .await;
    assert_eq!(ra["outcome"], "ready");
    assert_eq!(ra["facts"]["policy"]["minCriterionScore"], 3);
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.campus.governance_case.submitted",
            &sid(&ra)
        )
        .await,
        1
    );
    assert_eq!(
        outbox(&pool, "sutraerp.campus.governance_case.decided", &sid(&ra)).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/items", sid(&ra)),
        json!({"kind": "criterion", "title": "Late", "score": 5}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a closed case is frozen");

    // ================================================================ G-008 health assessment
    let ha = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "health_assessment", "title": "Operational health Q3"}),
    )
    .await;
    let ha = add_item(
        &app,
        &admin,
        &ha,
        json!({"kind": "criterion", "title": "Ticket backlog", "score": 2}),
    )
    .await;
    let (s, _) = submit(&app, &admin, &ha, json!({"outcome": "healthy"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let ha = close(&app, &admin, &ha, json!({"outcome": "at_risk"})).await;
    assert_eq!(ha["outcome"], "at_risk");

    // ================================================================ G-010 maturity assessment
    let ma = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "maturity_assessment", "title": "Facilities maturity"}),
    )
    .await;
    let ma = add_item(
        &app,
        &admin,
        &ma,
        json!({"kind": "criterion", "title": "Processes", "score": 4}),
    )
    .await;
    let ma = add_item(
        &app,
        &admin,
        &ma,
        json!({"kind": "criterion", "title": "Tooling", "score": 3}),
    )
    .await;
    let (s, _) = submit(&app, &admin, &ma, json!({"outcome": "level_4"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "3.5 is level 3");
    let ma = close(&app, &admin, &ma, json!({"outcome": "level_3"})).await;
    assert_eq!(ma["facts"]["maturityLevel"], "level_3");

    // ================================================================ G-003 operational review
    let or = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "operational_review", "title": "Monthly ops review"}),
    )
    .await;
    let or = add_item(
        &app,
        &admin,
        &or,
        json!({"kind": "action", "title": "Fix lift B2", "owner": "estate"}),
    )
    .await;
    let (s, _) = submit(&app, &admin, &or, json!({"outcome": "closed_with_actions"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "the action is open");
    let or = progress(
        &app,
        &admin,
        &or,
        0,
        json!({"status": "done", "note": "replaced cable"}),
    )
    .await;
    let or = close(&app, &admin, &or, json!({"outcome": "closed_with_actions"})).await;
    assert_eq!(or["items"][0]["status"], "done");

    // ================================================================ G-004 executive review
    let er = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "executive_review", "title": "Board review"}),
    )
    .await;
    let (s, _) = submit(
        &app,
        &admin,
        &er,
        json!({"outcome": "recommendations_accepted"}),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "no recommendation");
    let er = add_item(
        &app,
        &admin,
        &er,
        json!({"kind": "recommendation", "title": "Fund hostel refurbishment"}),
    )
    .await;
    let er = close(
        &app,
        &admin,
        &er,
        json!({"outcome": "recommendations_accepted"}),
    )
    .await;
    assert_eq!(er["outcome"], "recommendations_accepted");

    // ================================================================ G-005 risk register
    let (s, _) = call(
        &app,
        &admin,
        &cases,
        json!({"kind": "risk", "title": "Flooding"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a risk needs its rating"
    );
    let rk = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "risk", "title": "Basement flooding", "likelihood": 4, "impact": 4,
            "severity": "high", "domains": ["facility"]}),
    )
    .await;
    assert_eq!(rk["inherentScore"], 16);
    let rk = add_item(
        &app,
        &admin,
        &rk,
        json!({"kind": "action", "title": "Install sump pumps"}),
    )
    .await;
    let rk = progress(&app, &admin, &rk, 0, json!({"status": "done"})).await;
    let (id, v) = item(&rk, 0);
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-items/{id}/verify"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "verification needs evidence"
    );
    let (s, _) = submit(
        &app,
        &admin,
        &rk,
        json!({"outcome": "mitigated", "residualScore": 4}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the action is not verified"
    );
    let rk = verify(&app, &admin, &rk, 0).await;
    assert_eq!(rk["items"][0]["status"], "verified");
    let (s, _) = submit(
        &app,
        &admin,
        &rk,
        json!({"outcome": "mitigated", "residualScore": 9}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "above the effective risk appetite 8"
    );
    let rk = close(
        &app,
        &admin,
        &rk,
        json!({"outcome": "mitigated", "residualScore": 6}),
    )
    .await;
    assert_eq!(rk["residualScore"], 6);

    // ================================================================ G-006 multi-domain action plan
    let ap = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "action_plan", "title": "Monsoon readiness", "domains": ["facility", "hostel"]}),
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/items", sid(&ap)),
        json!({"kind": "action", "title": "No domain"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "action-plan actions name a domain"
    );
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/items", sid(&ap)),
        json!({"kind": "action", "title": "Wrong", "domain": "library"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "not a domain of the plan"
    );
    let ap = add_item(
        &app,
        &admin,
        &ap,
        json!({"kind": "action", "title": "Clear drains", "domain": "facility"}),
    )
    .await;
    let ap = progress(&app, &admin, &ap, 0, json!({"status": "done"})).await;
    let ap = verify(&app, &admin, &ap, 0).await;
    let (s, e) = submit(&app, &admin, &ap, json!({"outcome": "completed"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "hostel has no action: {e}"
    );
    let ap = add_item(
        &app,
        &admin,
        &ap,
        json!({"kind": "action", "title": "Seal hostel roofs", "domain": "hostel"}),
    )
    .await;
    let ap = progress(&app, &admin, &ap, 1, json!({"status": "done"})).await;
    let (s, _) = submit(&app, &admin, &ap, json!({"outcome": "completed"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "done is not verified");
    let ap = verify(&app, &admin, &ap, 1).await;
    close(&app, &admin, &ap, json!({"outcome": "completed"})).await;

    // ================================================================ G-007 SLA deviation
    let ticket = b8::seed_ticket(
        &pool,
        &setup,
        TicketCategory::Facility,
        TicketSource::Admin,
        None,
        None,
        "Broken projector",
    )
    .await;
    let ticket_id = ticket.id.as_uuid();
    sqlx::query(
        "UPDATE campus.service_ticket SET sla_due_at = now() - interval '2 hours' WHERE id = $1",
    )
    .bind(ticket_id)
    .execute(&pool)
    .await
    .unwrap();
    let (s, e) = call(
        &app,
        &admin,
        &cases,
        json!({"kind": "sla_deviation", "title": "Library SLA", "domains": ["library"]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no breach in library: {e}"
    );
    let sla = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "sla_deviation", "title": "Facility SLA breaches", "domains": ["facility"]}),
    )
    .await;
    assert_eq!(sla["facts"]["breached"], 1, "{sla}");
    assert_eq!(
        sla["facts"]["tickets"][0]["ticketId"],
        ticket_id.to_string()
    );
    let (s, e) = submit(&app, &admin, &sla, json!({"outcome": "resolved"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the ticket is open: {e}"
    );
    sqlx::query(
        "UPDATE campus.service_ticket SET status = 'resolved', resolved_at = now() WHERE id = $1",
    )
    .bind(ticket_id)
    .execute(&pool)
    .await
    .unwrap();
    let sla = ok(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/refresh", sid(&sla)),
        json!({}),
        Some(ver(&sla)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sla["facts"]["stillOpen"], 0);
    close(&app, &admin, &sla, json!({"outcome": "resolved"})).await;

    // ================================================================ G-009 KPI review
    let (s, _) = call(
        &app,
        &admin,
        &cases,
        json!({"kind": "kpi_review", "title": "Bad", "kpiTargets": [{"key": "finance.receivables", "target": "0"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "campus figures only");
    let kpi = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "kpi_review", "title": "October KPIs",
            "kpiTargets": [{"key": "campus.tickets_past_sla", "target": "0", "better": "lower"},
                           {"key": "campus.active_buildings", "target": "0", "better": "higher"}]}),
    )
    .await;
    assert_eq!(kpi["facts"]["offTrack"], true, "{kpi}");
    assert_eq!(kpi["facts"]["kpis"].as_array().unwrap().len(), 2);
    let (s, _) = submit(&app, &admin, &kpi, json!({"outcome": "on_track"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = submit(&app, &admin, &kpi, json!({"outcome": "off_track"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an off-track review needs an action"
    );
    let kpi = add_item(
        &app,
        &admin,
        &kpi,
        json!({"kind": "action", "title": "Daily SLA stand-up"}),
    )
    .await;
    close(&app, &admin, &kpi, json!({"outcome": "off_track"})).await;

    // ================================================================ G-011 finding → corrective action
    let fd = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "finding", "title": "Fire exits blocked", "severity": "high"}),
    )
    .await;
    let (s, _) = submit(
        &app,
        &admin,
        &fd,
        json!({"outcome": "no_action", "rationale": "x"}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a high finding needs a corrective action"
    );
    let fd = add_item(
        &app,
        &admin,
        &fd,
        json!({"kind": "corrective_action", "title": "Weekly exit audit"}),
    )
    .await;
    let fd = close(
        &app,
        &admin,
        &fd,
        json!({"outcome": "corrective_action_approved"}),
    )
    .await;
    assert_eq!(fd["items"][0]["kind"], "corrective_action");

    // ================================================================ G-012 escalation
    let (s, _) = call(
        &app,
        &admin,
        &cases,
        json!({"kind": "escalation", "title": "x"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "escalations come from a case"
    );
    let f2 = open_case(&app, &admin, inst, json!({"kind": "finding", "title": "Recurring water leaks", "severity": "medium", "domains": ["hostel"]})).await;
    let esc = ok(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/escalate", sid(&f2)),
        json!({"title": "Leaks need capital budget", "reason": "Three repairs failed"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (
            esc["kind"].as_str(),
            esc["status"].as_str(),
            esc["severity"].as_str()
        ),
        (Some("escalation"), Some("open"), Some("high"))
    );
    assert_eq!(esc["parentId"], f2["id"]);
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.campus.governance_case.escalated",
            &sid(&esc)
        )
        .await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/escalate", sid(&f2)),
        json!({"title": "Again", "reason": "again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live escalation per case");
    let (s, _) = submit(&app, &admin, &esc, json!({"outcome": "resolved"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an executive resolution needs a rationale"
    );
    close(
        &app,
        &admin,
        &esc,
        json!({"outcome": "resolved", "rationale": "Capital budget approved"}),
    )
    .await;

    // ================================================================ G-013 exception
    let (s, _) = call(
        &app,
        &admin,
        &cases,
        json!({"kind": "exception", "title": "Fire NOC pending", "exceptionType": "dependency"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an exception needs an expiry"
    );
    let ex = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "exception", "title": "Fire NOC pending", "exceptionType": "dependency",
            "expiresOn": iso(today() + Duration::days(30))}),
    )
    .await;
    let (s, _) = submit(&app, &admin, &ex, json!({"outcome": "granted"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let ex = close(
        &app,
        &admin,
        &ex,
        json!({"outcome": "granted", "rationale": "Municipal backlog"}),
    )
    .await;
    assert_eq!(ex["exceptionType"], "dependency");

    // ---- cancel needs a reason; a cancelled case is final.
    let cx = open_case(
        &app,
        &admin,
        inst,
        json!({"kind": "operational_review", "title": "Duplicate"}),
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/cancel", sid(&cx)),
        json!({}),
        Some(ver(&cx)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let cx = ok(
        &app,
        &admin,
        &format!("{C}/governance-cases/{}/cancel", sid(&cx)),
        json!({"reason": "duplicate"}),
        Some(ver(&cx)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cx["status"], "cancelled");

    // ================================================================ G-002 / G-015 evidence
    let ev_path = format!("{C}/institutions/{inst}/governance-evidence");
    let (s, _) = call(&app, &admin, &ev_path, json!({"caseId": f2["id"]}), None).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "only a closed case becomes evidence"
    );
    let ev = ok(
        &app,
        &admin,
        &ev_path,
        json!({"caseId": ra["id"], "summary": "Readiness for the odd semester"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (
            ev["status"].as_str(),
            ev["accreditationReady"].as_bool(),
            ev["caseNumber"].as_str()
        ),
        (Some("draft"), Some(false), ra["number"].as_str())
    );
    assert!(ev["facts"]["campusFigures"].as_array().unwrap().len() >= 4);
    let ev = ok(
        &app,
        &admin,
        &format!("{C}/governance-evidence/{}/publish", sid(&ev)),
        json!({}),
        Some(ver(&ev)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ev["accreditationReady"], true);
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.campus.governance_evidence.published",
            &sid(&ev)
        )
        .await,
        1
    );
    let data: Value = sqlx::query_scalar(
        "SELECT envelope->'data' FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind("sutraerp.campus.governance_evidence.published")
    .bind(sid(&ev))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(data["caseNumber"], ra["number"]);
    assert_eq!(data["caseOutcome"], "ready");
    let (s, _) = call(
        &app,
        &admin,
        &format!("{C}/governance-evidence/{}/publish", sid(&ev)),
        json!({}),
        Some(ver(&ev)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "published once");
    let (s, list) = read(&app, &admin, &format!("{ev_path}?status=published")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // ---- lists, history and audit.
    let (_, closed) = read(&app, &admin, &format!("{cases}?status=closed")).await;
    assert!(closed["items"].as_array().unwrap().len() >= 12, "{closed}");
    let (_, risks) = read(&app, &admin, &format!("{cases}?kind=risk")).await;
    assert_eq!(risks["items"].as_array().unwrap().len(), 1);
    let (s, _) = read(&app, &admin, &format!("{cases}?kind=bogus")).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.status_history WHERE aggregate_type = 'governance_case' AND aggregate_id = $1",
    )
    .bind(Uuid::parse_str(&sid(&rk)).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 4, "draft, open, pending_decision, closed");
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1 AND action LIKE 'campus.governance_%'",
    )
    .bind(Uuid::parse_str(TENANT).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(audits >= 40, "{audits}");
    let rk2 = reread(&app, &admin, &rk).await;
    assert_eq!(rk2["decidedBy"], "system:auto-approval");
}

// ==================================================================== G00 AwaitDecision

fn auth(subject: &str, role: Role, tenant: TenantId) -> AuthContext {
    AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![role],
        scope: ExecutionScope::tenant(tenant),
        csrf_token: "a".repeat(64),
        expires_at: Utc::now(),
        idle_expires_at: Utc::now(),
        mfa_level: MfaLevel::None,
        amr: vec!["pwd".into()],
        step_up_enforced: false,
    }
}

async fn hit(
    r: &axum::Router,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::now_v7().to_string());
    if let Some(v) = if_match {
        b = b.header("if-match", format!("\"{v}\""));
    }
    let (s, _, v) = send(r, b.body(Body::from(body.to_string())).unwrap()).await;
    (s, v)
}

#[tokio::test]
async fn governance_await_decision() {
    let (app, pool) = app(TENANT_AWAIT).await;
    let admin_login = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(
        &app,
        &admin_login,
        &pool,
        TENANT_AWAIT,
        "Governance Approvals",
    )
    .await;
    let t = setup.tenant();
    let inst = setup.institution().as_uuid();
    let mut st = sutra_api::app::campus_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let as_user = |who: AuthContext| {
        let (router, _) = sutra_campus::router_with_state(st.clone()).split_for_parts();
        router.layer(axum::Extension(who))
    };
    let admin = as_user(auth(ADMIN, Role::Administrator, t));
    let meera = as_user(auth(b4::MEERA, Role::Faculty, t));

    let (s, c) = hit(
        &admin,
        &format!("{C}/institutions/{inst}/governance-cases"),
        json!({"kind": "health_assessment", "title": "Hostel health"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let (s, c) = hit(
        &admin,
        &format!("{C}/governance-cases/{}/start", sid(&c)),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let (s, c) = hit(
        &admin,
        &format!("{C}/governance-cases/{}/items", sid(&c)),
        json!({"kind": "criterion", "title": "Water supply", "score": 4}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let (s, c) = hit(
        &admin,
        &format!("{C}/governance-cases/{}/submit", sid(&c)),
        json!({"outcome": "healthy"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        (
            c["status"].as_str(),
            c["approvalState"].as_str(),
            c["outcome"].as_str()
        ),
        (Some("pending_decision"), Some("pending"), Some("healthy"))
    );
    assert!(c["processInstanceId"].is_string());
    let path = format!("{C}/governance-cases/{}/decision", sid(&c));
    let (s, _) = hit(&meera, &path, json!({"decision": "approve"}), Some(ver(&c))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "faculty do not decide");
    let (s, _) = hit(&admin, &path, json!({"decision": "reject"}), Some(ver(&c))).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a reason"
    );
    let (s, c) = hit(
        &admin,
        &path,
        json!({"decision": "reject", "reason": "Rate the mess too"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        (c["status"].as_str(), c["approvalState"].as_str()),
        (Some("open"), Some("rejected"))
    );
    assert!(c["outcome"].is_null());
    let (s, _) = hit(&admin, &path, json!({"decision": "approve"}), Some(ver(&c))).await;
    assert_eq!(s, StatusCode::CONFLICT, "not pending any more");
    let (_, c) = hit(
        &admin,
        &format!("{C}/governance-cases/{}/items", sid(&c)),
        json!({"kind": "criterion", "title": "Mess hygiene", "score": 3}),
        None,
    )
    .await;
    let (s, c) = hit(
        &admin,
        &format!("{C}/governance-cases/{}/submit", sid(&c)),
        json!({"outcome": "healthy"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let (s, _) = hit(
        &admin,
        &path,
        json!({"decision": "approve"}),
        Some(ver(&c) - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale ETag");
    let (s, c) = hit(
        &admin,
        &path,
        json!({"decision": "approve", "reason": "ok"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        (
            c["status"].as_str(),
            c["approvalState"].as_str(),
            c["decidedBy"].as_str()
        ),
        (Some("closed"), Some("approved"), Some(ADMIN))
    );
    assert_eq!(
        outbox(&pool, "sutraerp.campus.governance_case.decided", &sid(&c)).await,
        2
    );

    // ---- a configuration change awaits its decision.
    let (s, cfg) = hit(
        &admin,
        &format!("{C}/institutions/{inst}/governance-configs"),
        json!({"key": "readiness_threshold", "settings": {"minCriterionScore": 4}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{cfg}");
    let (s, cfg) = hit(
        &admin,
        &format!("{C}/governance-configs/{}/submit", sid(&cfg)),
        json!({}),
        Some(ver(&cfg)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cfg}");
    assert_eq!(
        (cfg["status"].as_str(), cfg["approvalState"].as_str()),
        (Some("draft"), Some("pending"))
    );
    let (s, cfg) = hit(
        &admin,
        &format!("{C}/governance-configs/{}/decision", sid(&cfg)),
        json!({"decision": "approve"}),
        Some(ver(&cfg)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cfg}");
    assert_eq!(cfg["status"], "effective");
    let p = sutra_campus::governance::policy::effective_policy(&pool, t, setup.institution())
        .await
        .unwrap();
    assert_eq!(p.min_criterion_score, 4);
}

// ==================================================================== H09 through the API

async fn rec(app: &axum::Router, who: &Login, inst: Uuid, body: Value) -> Value {
    let r = ok(
        app,
        who,
        &format!("{S}/institutions/{inst}/accreditation-records"),
        body,
        None,
        StatusCode::CREATED,
    )
    .await;
    assert!(r["number"].as_str().unwrap().starts_with("ACR-"));
    ok(
        app,
        who,
        &format!("{S}/accreditation-records/{}/start", sid(&r)),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await
}

async fn rec_item(app: &axum::Router, who: &Login, r: &Value, body: Value) -> Value {
    ok(
        app,
        who,
        &format!("{S}/accreditation-records/{}/items", sid(r)),
        body,
        None,
        StatusCode::CREATED,
    )
    .await
}

async fn rec_done(app: &axum::Router, who: &Login, r: &Value, n: usize) -> Value {
    let (id, v) = item(r, n);
    ok(
        app,
        who,
        &format!("{S}/accreditation-items/{id}/progress"),
        json!({"status": "done"}),
        Some(v),
        StatusCode::OK,
    )
    .await
}

async fn rec_verify(app: &axum::Router, who: &Login, r: &Value, n: usize) -> Value {
    let (id, v) = item(r, n);
    ok(
        app,
        who,
        &format!("{S}/accreditation-items/{id}/verify"),
        json!({"evidenceRef": "IQAC/2026/17"}),
        Some(v),
        StatusCode::OK,
    )
    .await
}

async fn rec_submit(
    app: &axum::Router,
    who: &Login,
    r: &Value,
    body: Value,
) -> (StatusCode, Value) {
    call(
        app,
        who,
        &format!("{S}/accreditation-records/{}/submit", sid(r)),
        body,
        Some(ver(r)),
    )
    .await
}

async fn rec_close(app: &axum::Router, who: &Login, r: &Value, body: Value) -> Value {
    let (s, x) = rec_submit(app, who, r, body).await;
    assert_eq!(s, StatusCode::OK, "{x}");
    assert_eq!(
        (x["status"].as_str(), x["approvalState"].as_str()),
        (Some("closed"), Some("approved")),
        "{x}"
    );
    x
}

fn criteria() -> Value {
    json!([
        {"code": "C1", "title": "Curricular aspects", "weight": 60, "metricKeys": ["people.working_staff"]},
        {"code": "C2", "title": "Infrastructure and learning resources", "weight": 40, "metricKeys": ["campus.active_spaces"]}
    ])
}

#[tokio::test]
async fn accreditation_h061_to_h071_through_the_api() {
    let (app, pool) = app(TENANT_ACCR).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera_login = login(&app, "meera.shah@sutra.dev").await;
    let setup =
        b8::campus_setup(&app, &admin, &pool, TENANT_ACCR, "Accreditation University").await;
    let inst = setup.institution().as_uuid();
    b8::seed_staff(&pool, &setup, "Meera", Some(b4::MEERA)).await;
    let fw_path = format!("{S}/institutions/{inst}/accreditation-frameworks");

    // ---- permissions.
    let (s, _) = read(&app, &meera_login, &fw_path).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ================================================================ H-061 framework versions
    let (s, e) = call(
        &app,
        &admin,
        &fw_path,
        json!({"code": "NAAC-2025", "name": "NAAC A&A", "body": "NAAC",
            "criteria": [{"code": "C1", "title": "x", "weight": 90}]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "weights add up to 100: {e}"
    );
    let (s, _) = call(
        &app,
        &admin,
        &fw_path,
        json!({"code": "NAAC-2025", "name": "NAAC A&A", "body": "NAAC",
            "criteria": [{"code": "C1", "title": "x", "weight": 100, "metricKeys": ["people.nope"]}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown figure");
    let (s, _) = call(
        &app,
        &admin,
        &fw_path,
        json!({"code": "NAAC-2025", "name": "NAAC A&A", "body": "UGC", "criteria": criteria()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown body");
    let f1 = ok(
        &app,
        &admin,
        &fw_path,
        json!({"code": "naac-2025", "name": "NAAC A&A", "body": "naac",
            "criteria": [{"code": "C1", "title": "All", "weight": 100}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (
            f1["code"].as_str(),
            f1["body"].as_str(),
            f1["approvalState"].as_str()
        ),
        (Some("NAAC-2025"), Some("NAAC"), Some("not_requested"))
    );
    let f1 = ok(
        &app,
        &admin,
        &format!("{S}/accreditation-frameworks/{}/submit", sid(&f1)),
        json!({}),
        Some(ver(&f1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f1["status"], "active", "{f1}");
    let f2 = ok(
        &app,
        &admin,
        &fw_path,
        json!({"code": "NAAC-2025", "name": "NAAC A&A (revised)", "body": "NAAC", "criteria": criteria()}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (f2["versionNo"].as_i64(), f2["predecessorId"].as_str()),
        (Some(2), f1["id"].as_str())
    );
    let f2 = ok(
        &app,
        &admin,
        &format!("{S}/accreditation-frameworks/{}/submit", sid(&f2)),
        json!({}),
        Some(ver(&f2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f2["status"], "active");
    let (_, f1) = read(
        &app,
        &admin,
        &format!("{S}/accreditation-frameworks/{}", sid(&f1)),
    )
    .await;
    assert_eq!(f1["status"], "superseded");
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.services.accreditation_framework.activated",
            &sid(&f2)
        )
        .await,
        1
    );

    // ================================================================ H-062 cycle + plan
    let cy_path = format!("{S}/institutions/{inst}/accreditation-cycles");
    let period = |f: &Value| {
        json!({"frameworkId": f["id"], "title": "NAAC cycle 2026-27",
        "periodStart": iso(today() - Duration::days(30)), "periodEnd": iso(today() + Duration::days(300))})
    };
    let (s, _) = call(&app, &admin, &cy_path, period(&f1), None).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a superseded framework opens no cycle"
    );
    let cy = ok(
        &app,
        &admin,
        &cy_path,
        period(&f2),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (cy["status"].as_str(), cy["frameworkVersionNo"].as_i64()),
        (Some("planning"), Some(2))
    );
    assert!(cy["number"].as_str().unwrap().starts_with("AC-"));
    let due = iso(today() + Duration::days(60));
    let (s, e) = call(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/plan", sid(&cy)),
        json!({"entries": [{"criterionCode": "C1", "owner": "IQAC", "dueOn": due}]}),
        Some(ver(&cy)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "C2 unplanned: {e}");
    let (s, _) = call(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/plan", sid(&cy)),
        json!({"entries": [{"criterionCode": "C1", "owner": "IQAC", "dueOn": due},
            {"criterionCode": "C2", "owner": "Estate", "dueOn": iso(today() + Duration::days(900))}]}),
        Some(ver(&cy)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "due outside the period"
    );
    let (s, _) = call(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/submit", sid(&cy)),
        json!({}),
        Some(ver(&cy)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "no plan yet");
    let cy = ok(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/plan", sid(&cy)),
        json!({"entries": [{"criterionCode": "c1", "owner": "IQAC", "dueOn": due},
            {"criterionCode": "C2", "owner": "Estate", "dueOn": due}]}),
        Some(ver(&cy)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cy["plan"].as_array().unwrap().len(), 2);
    let cy = ok(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/submit", sid(&cy)),
        json!({}),
        Some(ver(&cy)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (cy["status"].as_str(), cy["approvalState"].as_str()),
        (Some("active"), Some("approved")),
        "{cy}"
    );
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.services.accreditation_cycle.plan_approved",
            &sid(&cy)
        )
        .await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/plan", sid(&cy)),
        json!({"entries": []}),
        Some(ver(&cy)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the approved plan is frozen");
    let cycle_id = cy["id"].clone();

    // ================================================================ H-063 obligations
    let ob = rec(&app, &admin, inst, json!({"kind": "obligation", "title": "AISHE annual return", "dueOn": iso(today() + Duration::days(20))})).await;
    let (s, _) = rec_submit(&app, &admin, &ob, json!({"outcome": "compliant"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "no requirement yet");
    let ob = rec_item(
        &app,
        &admin,
        &ob,
        json!({"kind": "requirement", "title": "Upload DCF-II"}),
    )
    .await;
    let ob = rec_done(&app, &admin, &ob, 0).await;
    let (s, _) = rec_submit(&app, &admin, &ob, json!({"outcome": "compliant"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "requirements must be verified"
    );
    let ob = rec_verify(&app, &admin, &ob, 0).await;
    rec_close(&app, &admin, &ob, json!({"outcome": "compliant"})).await;
    // an overdue open obligation for the periodic review
    let late = ok(
        &app,
        &admin,
        &format!("{S}/institutions/{inst}/accreditation-records"),
        json!({"kind": "obligation", "title": "Fire safety audit", "dueOn": iso(today() - Duration::days(3))}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(late["status"], "draft");

    // ================================================================ H-070 periodic compliance review
    let cr = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "compliance_review", "title": "Q3 compliance review"}),
    )
    .await;
    let (s, e) = rec_submit(&app, &admin, &cr, json!({"outcome": "compliant"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "one obligation is overdue: {e}"
    );
    let cr = rec_close(&app, &admin, &cr, json!({"outcome": "partially_compliant"})).await;
    assert_eq!(cr["facts"]["obligations"]["overdueOpen"], 1);

    // ================================================================ H-064 evidence
    let rp = format!("{S}/institutions/{inst}/accreditation-records");
    let (s, _) = call(
        &app,
        &admin,
        &rp,
        json!({"kind": "evidence", "title": "x", "criterionCode": "C1"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "evidence belongs to a cycle"
    );
    let (s, _) = call(
        &app,
        &admin,
        &rp,
        json!({"kind": "evidence", "title": "x", "cycleId": cycle_id, "criterionCode": "C9"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown criterion");
    let (s, _) = call(
        &app,
        &admin,
        &rp,
        json!({"kind": "evidence", "title": "x", "cycleId": cycle_id, "criterionCode": "C1",
            "regulatorySnapshotIds": [Uuid::now_v7()]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the snapshot is not a validated one"
    );
    let gov_evidence = Uuid::now_v7();
    let e1 = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "evidence", "title": "Faculty profile 2025-26", "cycleId": cycle_id,
            "criterionCode": "C1", "governanceEvidenceIds": [gov_evidence],
            "sourceUrl": "https://dms.example.edu/iqac/c1"}),
    )
    .await;
    assert_eq!(
        e1["refs"]["governanceEvidenceIds"][0],
        gov_evidence.to_string()
    );
    let e1 = rec_close(&app, &admin, &e1, json!({"outcome": "approved_evidence"})).await;
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.services.accreditation_record.decided",
            &sid(&e1)
        )
        .await,
        1
    );

    // ================================================================ H-069 readiness (blocked)
    let rd = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "readiness", "title": "SSR readiness", "cycleId": cycle_id}),
    )
    .await;
    let (s, e) = rec_submit(&app, &admin, &rd, json!({"outcome": "ready"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "C2 has no evidence: {e}"
    );
    let e2 = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "evidence", "title": "Space audit", "cycleId": cycle_id, "criterionCode": "C2",
            "sourceUrl": "https://dms.example.edu/iqac/c2"}),
    )
    .await;
    rec_close(&app, &admin, &e2, json!({"outcome": "approved_evidence"})).await;

    // ================================================================ H-065 internal audit + H-067 CAPA
    let au = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "internal_audit", "title": "AQAR internal audit", "cycleId": cycle_id}),
    )
    .await;
    let au = rec_item(
        &app,
        &admin,
        &au,
        json!({"kind": "checklist", "title": "Lab safety registers"}),
    )
    .await;
    let (s, _) = rec_submit(&app, &admin, &au, json!({"outcome": "closed_no_findings"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "the checklist is open");
    let fi = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "finding", "title": "Lab registers incomplete", "severity": "high", "parentId": au["id"]}),
    )
    .await;
    assert_eq!(fi["cycleId"], cycle_id, "inherits the audit's cycle");
    let (s, e) = rec_submit(&app, &admin, &rd, json!({"outcome": "ready"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an open high finding blocks readiness: {e}"
    );
    let (s, _) = rec_submit(
        &app,
        &admin,
        &fi,
        json!({"outcome": "no_action", "rationale": "x"}),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a high finding needs CAPA"
    );
    let fi = rec_item(
        &app,
        &admin,
        &fi,
        json!({"kind": "action", "title": "Weekly register check"}),
    )
    .await;
    let fi = rec_done(&app, &admin, &fi, 0).await;
    let (s, _) = rec_submit(&app, &admin, &fi, json!({"outcome": "capa_verified"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "CAPA not verified");
    let fi = rec_verify(&app, &admin, &fi, 0).await;
    rec_close(&app, &admin, &fi, json!({"outcome": "capa_verified"})).await;
    let au = rec_done(&app, &admin, &au, 0).await;
    let (s, _) = rec_submit(&app, &admin, &au, json!({"outcome": "closed_no_findings"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a finding was raised");
    let au = rec_close(
        &app,
        &admin,
        &au,
        json!({"outcome": "closed_with_findings"}),
    )
    .await;
    assert_eq!(au["facts"]["findings"], 1);

    // ================================================================ H-066 external assessment
    let ea = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "external_assessment", "title": "Peer team visit", "cycleId": cycle_id}),
    )
    .await;
    let (s, _) = rec_submit(&app, &admin, &ea, json!({"outcome": "accepted"})).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "the grade is required");
    let ea = rec_close(
        &app,
        &admin,
        &ea,
        json!({"outcome": "accepted", "grade": "A+ (3.42)"}),
    )
    .await;
    assert_eq!(ea["facts"]["grade"], "A+ (3.42)");

    // ================================================================ H-068 quality improvement
    let (s, _) = call(
        &app,
        &admin,
        &rp,
        json!({"kind": "improvement", "title": "x", "metricKey": "campus.active_spaces"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "direction required");
    let qi = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "improvement", "title": "More learning spaces", "metricKey": "campus.active_spaces", "better": "higher"}),
    )
    .await;
    assert_eq!(qi["facts"]["baseline"], "0");
    let qi = rec_item(
        &app,
        &admin,
        &qi,
        json!({"kind": "action", "title": "Commission block C"}),
    )
    .await;
    let qi = rec_done(&app, &admin, &qi, 0).await;
    let (s, e) = rec_submit(&app, &admin, &qi, json!({"outcome": "effective"})).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the figure has not moved: {e}"
    );
    b8::seed_hierarchy(
        &pool,
        &setup,
        "BLKC",
        &[
            ("C101", SpaceType::Classroom, 60),
            ("C102", SpaceType::Classroom, 60),
        ],
    )
    .await;
    let qi = rec_close(&app, &admin, &qi, json!({"outcome": "effective"})).await;
    assert_eq!(qi["facts"]["current"], "2");

    // ================================================================ H-069 readiness decision
    let rd = rec_close(&app, &admin, &rd, json!({"outcome": "ready"})).await;
    assert_eq!(rd["facts"]["coverageMissing"].as_array().unwrap().len(), 0);

    // ================================================================ H-071 self-study report
    let ss = rec(
        &app,
        &admin,
        inst,
        json!({"kind": "self_study", "title": "Self-study report 2026", "cycleId": cycle_id}),
    )
    .await;
    let ss = rec_close(&app, &admin, &ss, json!({"outcome": "approved"})).await;
    assert_eq!(ss["facts"]["approvedEvidence"].as_array().unwrap().len(), 2);
    let (s, _) = call(
        &app,
        &admin,
        &format!(
            "{S}/accreditation-cycles/{}/close",
            cycle_id.as_str().unwrap()
        ),
        json!({}),
        Some(ver(&cy)),
    )
    .await;
    assert!(
        matches!(s, StatusCode::CONFLICT | StatusCode::PRECONDITION_FAILED),
        "not handed off yet"
    );
    let ss = ok(
        &app,
        &admin,
        &format!("{S}/accreditation-records/{}/handoff", sid(&ss)),
        json!({"reference": "NAAC-SSR-2026-00917"}),
        Some(ver(&ss)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ss["handoffReference"], "NAAC-SSR-2026-00917");
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.services.accreditation_record.handed_off",
            &sid(&ss)
        )
        .await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        &format!("{S}/accreditation-records/{}/handoff", sid(&ss)),
        json!({"reference": "again"}),
        Some(ver(&ss)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "handed off once");
    let (_, cy) = read(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}", cycle_id.as_str().unwrap()),
    )
    .await;
    assert_eq!(cy["status"], "submitted", "{cy}");
    let cy = ok(
        &app,
        &admin,
        &format!("{S}/accreditation-cycles/{}/close", sid(&cy)),
        json!({}),
        Some(ver(&cy)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cy["status"], "closed");
    assert!(cy["records"].as_array().unwrap().len() >= 6);

    // ---- lists + audit.
    let (_, ev) = read(&app, &admin, &format!("{rp}?kind=evidence&status=closed")).await;
    assert_eq!(ev["items"].as_array().unwrap().len(), 2);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1 AND action LIKE 'services.accreditation_%'",
    )
    .bind(Uuid::parse_str(TENANT_ACCR).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(audits >= 40, "{audits}");

    // ================================================================ §6.4 approvalState (8D)
    let sup = ok(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Acme Traders", "pan": "ABCDE1234F", "email": "sales@example.in"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (
            sup["approvalState"].as_str(),
            sup["approvalPending"].as_bool()
        ),
        (Some("not_requested"), Some(false))
    );
}

// ==================================================================== H09 AwaitDecision

#[tokio::test]
async fn accreditation_await_decision() {
    let (app, pool) = app(TENANT_ACCR_AWAIT).await;
    let admin_login = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(
        &app,
        &admin_login,
        &pool,
        TENANT_ACCR_AWAIT,
        "Accreditation Approvals",
    )
    .await;
    let t = setup.tenant();
    let inst = setup.institution().as_uuid();
    let mut st = sutra_api::app::services_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = sutra_services::shared::approval::ApprovalMode::AwaitDecision;
    let as_user = |who: AuthContext| {
        let (router, _) = sutra_services::router_with_state(st.clone()).split_for_parts();
        router.layer(axum::Extension(who))
    };
    let admin = as_user(auth(ADMIN, Role::Administrator, t));
    let meera = as_user(auth(b4::MEERA, Role::Faculty, t));

    let (s, f) = hit(
        &admin,
        &format!("{S}/institutions/{inst}/accreditation-frameworks"),
        json!({"code": "NBA-T1", "name": "NBA Tier I", "body": "NBA",
            "criteria": [{"code": "C1", "title": "Vision and mission", "weight": 100}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{f}");
    let (s, f) = hit(
        &admin,
        &format!("{S}/accreditation-frameworks/{}/submit", sid(&f)),
        json!({}),
        Some(ver(&f)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!(
        (f["status"].as_str(), f["approvalState"].as_str()),
        (Some("draft"), Some("pending"))
    );
    let path = format!("{S}/accreditation-frameworks/{}/decision", sid(&f));
    let (s, _) = hit(&meera, &path, json!({"decision": "approve"}), Some(ver(&f))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = hit(&admin, &path, json!({"decision": "reject"}), Some(ver(&f))).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a reason"
    );
    let (s, f) = hit(
        &admin,
        &path,
        json!({"decision": "reject", "reason": "Add PO mapping"}),
        Some(ver(&f)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!(f["approvalState"], "rejected");
    let (s, f) = hit(
        &admin,
        &format!("{S}/accreditation-frameworks/{}/submit", sid(&f)),
        json!({}),
        Some(ver(&f)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{f}");
    let (s, f) = hit(&admin, &path, json!({"decision": "approve"}), Some(ver(&f))).await;
    assert_eq!(s, StatusCode::OK, "{f}");
    assert_eq!(
        (f["status"].as_str(), f["approvalState"].as_str()),
        (Some("active"), Some("approved"))
    );

    // ---- a record decision waits for services.report.approve.
    let (s, r) = hit(
        &admin,
        &format!("{S}/institutions/{inst}/accreditation-records"),
        json!({"kind": "finding", "title": "Minor documentation gap", "severity": "low"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let (_, r) = hit(
        &admin,
        &format!("{S}/accreditation-records/{}/start", sid(&r)),
        json!({}),
        Some(ver(&r)),
    )
    .await;
    let (s, r) = hit(
        &admin,
        &format!("{S}/accreditation-records/{}/submit", sid(&r)),
        json!({"outcome": "no_action", "rationale": "Cosmetic"}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(
        (r["status"].as_str(), r["approvalState"].as_str()),
        (Some("pending_decision"), Some("pending"))
    );
    let rpath = format!("{S}/accreditation-records/{}/decision", sid(&r));
    let (s, _) = hit(
        &admin,
        &rpath,
        json!({"decision": "approve"}),
        Some(ver(&r) + 3),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, r) = hit(
        &admin,
        &rpath,
        json!({"decision": "approve"}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(
        (r["status"].as_str(), r["decidedBy"].as_str()),
        (Some("closed"), Some(ADMIN))
    );
}
