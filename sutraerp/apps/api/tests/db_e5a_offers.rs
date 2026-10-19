//! ERP_EXTENSIONS 5A — offers, onboarding checklists, follow-ups, hiring analytics and the public
//! careers page over the HTTP API against PostgreSQL.
//!
//! * `offer_to_onboarding_followups_analytics` — offer draft (CTC breakup, employer PF default,
//!   validation), one offer in play, submit (dev auto-approval), send (letter snapshot,
//!   candidate → offered), accept (candidate → offer_accepted with the joining date), the existing
//!   onboarding case stamps its checklist, tasks tick off, follow-ups come due, the attention view
//!   and analytics read the funnel, decline / withdraw.
//! * `offer_approval_needs_a_second_person` — `ApprovalMode::AwaitDecision`: the drafter /
//!   submitter cannot decide (403), another approver can; send-back returns to draft.
//! * `public_careers_page_lists_and_applies` — slug settings, publishing, the unauthenticated
//!   list (safe fields), apply (honeypot, duplicate, size limit, validation), 404s.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b7;
use support::*;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_people::shared::approval::ApprovalMode;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000e5a00";
const TENANT_SOD: &str = "0192f5a0-7c3e-7000-8000-0000000e5a01";
const TENANT_PUB: &str = "0192f5a0-7c3e-7000-8000-0000000e5a02";
const P: &str = "/api/v1/people";

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, v)).await;
    (s, b)
}

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, v).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn ver(v: &Value) -> Option<i64> {
    v["version"].as_i64()
}

fn num(v: &Value) -> f64 {
    v.as_str()
        .map(|s| s.parse::<f64>().unwrap())
        .or_else(|| v.as_f64())
        .unwrap_or(f64::NAN)
}

async fn seed_requisition(
    pool: &sqlx::PgPool,
    t: Uuid,
    inst: Uuid,
    number: &str,
    openings: i32,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO people.requisition (tenant_id, institution_id, requisition_number, title, staff_category, \
         employment_type, designation, openings, status) VALUES ($1, $2, $3, 'Assistant Professor, Commerce', \
         'teaching', 'permanent', 'Assistant Professor', $4, 'open') RETURNING id",
    )
    .bind(t)
    .bind(inst)
    .bind(number)
    .bind(openings)
    .fetch_one(pool)
    .await
    .expect("requisition")
}

async fn seed_candidate(
    pool: &sqlx::PgPool,
    t: Uuid,
    inst: Uuid,
    req: Uuid,
    name: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO people.candidate (tenant_id, institution_id, requisition_id, given_name, email, status, source) \
         VALUES ($1, $2, $3, $4, $5, $6, 'Referral') RETURNING id",
    )
    .bind(t)
    .bind(inst)
    .bind(req)
    .bind(name)
    .bind(format!("{}@applicants.sutra.dev", name.to_lowercase()))
    .bind(status)
    .fetch_one(pool)
    .await
    .expect("candidate")
}

#[tokio::test]
async fn offer_to_onboarding_followups_analytics() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Offer College").await;
    let inst = setup.institution().as_uuid();
    let t = setup.tenant().as_uuid();
    let req = seed_requisition(&pool, t, inst, "REQ-005001", 1).await;
    let today = chrono::Utc::now().date_naive();
    let join = today + chrono::Duration::days(60);
    let expires = today + chrono::Duration::days(30);
    let late = join + chrono::Duration::days(14);

    // A candidate through the API (status history feeds the funnel), shortlisted.
    let priya = ok(&app, &admin, "POST", &format!("{P}/requisitions/{req}/candidates"),
        json!({"givenName": "Priya", "familyName": "Kulkarni", "email": "priya@applicants.sutra.dev", "source": "Naukri"}),
        None, StatusCode::CREATED).await;
    let pid = priya["id"].as_str().unwrap().to_owned();
    // Offers go to shortlisted / interviewed candidates.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{pid}/offers"),
        json!({"joiningDate": join, "ctc": {"basic": "57700"}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{pid}/shortlist"),
        json!({}),
        ver(&priya),
        StatusCode::OK,
    )
    .await;

    // Validation: expiry after joining, negative bonus, PF above 12 %.
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/candidates/{pid}/offers"),
        json!({"joiningDate": join, "expiresOn": late, "ctc": {"basic": "57700", "employerPf": "20000"},
               "joiningBonus": "-5"}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let text = b.to_string();
    assert!(
        text.contains("expiry_after_joining")
            && text.contains("bonus_negative")
            && text.contains("pf_above_statutory"),
        "{b}"
    );

    // Draft: designation from the requisition; employer PF defaults to 12 % of ₹15,000.
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{pid}/offers"),
        json!({"joiningDate": join, "expiresOn": expires, "probationMonths": 12,
               "department": "Department of Commerce", "grade": "Academic Level 10",
               "ctc": {"basic": "57700", "hra": "5770", "da": "0", "allowances": "2000"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(o["status"], "draft");
    assert_eq!(o["designation"], "Assistant Professor");
    assert_eq!(num(&o["ctc"]["employerPf"]), 1800.0);
    assert_eq!(num(&o["ctcMonthly"]), 67_270.0);
    assert_eq!(num(&o["ctcAnnual"]), 807_240.0);
    assert!(o["number"].as_str().unwrap().starts_with("OFR-"));
    let oid = o["id"].as_str().unwrap().to_owned();
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{pid}/offers"),
        json!({"joiningDate": join, "ctc": {"basic": "50000"}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one offer in play");

    // Edit the draft, preview the letter, submit (auto-approval in this suite), send.
    let o = ok(&app, &admin, "PUT", &format!("{P}/offers/{oid}"),
        json!({"joiningDate": join, "expiresOn": expires, "probationMonths": 12,
               "department": "Department of Commerce", "grade": "Academic Level 10",
               "ctc": {"basic": "57700", "hra": "5770", "allowances": "2000"}, "joiningBonus": "25000"}),
        ver(&o), StatusCode::OK).await;
    let preview = fetch(&app, &admin, &format!("{P}/offers/{oid}/letter")).await;
    assert_eq!(preview["snapshot"], false);
    let letter = preview["text"].as_str().unwrap();
    assert!(letter.contains("Dear Priya Kulkarni"), "{letter}");
    assert!(letter.contains("₹8,07,240"), "{letter}");
    assert!(
        letter.contains("Rupees Eight Lakh Seven Thousand Two Hundred Forty only"),
        "{letter}"
    );
    assert!(letter.contains("₹25,000 (one time)"), "{letter}");
    assert!(
        !letter.contains("Dearness allowance"),
        "zero DA line dropped: {letter}"
    );
    assert!(preview["html"]
        .as_str()
        .unwrap()
        .contains("<p class=\"terms\">"));
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{oid}/send"),
        json!({}),
        ver(&o),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "draft cannot be sent");
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{oid}/submit"),
        json!({}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "approved");
    assert_eq!(o["decidedBy"], "system:auto-approval");
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{oid}/send"),
        json!({}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "sent");
    let c = fetch(&app, &admin, &format!("{P}/candidates/{pid}")).await;
    assert_eq!(c["status"], "offered");
    let sent = fetch(&app, &admin, &format!("{P}/offers/{oid}/letter")).await;
    assert_eq!(sent["snapshot"], true);

    // Follow-ups: a past one is overdue and shows in the attention view with the expiring offer.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{pid}/followups"),
        json!({"text": " ", "dueDate": "soon"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let f = ok(&app, &admin, "POST", &format!("{P}/candidates/{pid}/followups"),
        json!({"text": "Confirm notice period with current college", "dueDate": today - chrono::Duration::days(2)}),
        None, StatusCode::CREATED).await;
    assert_eq!(f["overdue"], true);
    let list = fetch(&app, &admin, &format!("{P}/candidates/{pid}/followups")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let att = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/hiring-followups"),
    )
    .await;
    assert!(
        att["reminders"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "followup"),
        "{att}"
    );
    let fid = f["id"].as_str().unwrap();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidate-followups/{fid}/complete"),
        json!({"done": true}),
        None,
        StatusCode::NO_CONTENT,
    )
    .await;

    // Accept: candidate → offer_accepted with the joining date; onboarding stamps the checklist.
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{oid}/accept"),
        json!({"acceptedByName": "Priya Kulkarni"}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "accepted");
    let c = fetch(&app, &admin, &format!("{P}/candidates/{pid}")).await;
    assert_eq!(c["status"], "offer_accepted");
    assert_eq!(c["proposedJoiningDate"], join.to_string());
    assert_eq!(c["offeredDesignation"], "Assistant Professor");
    let signed = fetch(&app, &admin, &format!("{P}/offers/{oid}/letter")).await;
    assert!(signed["text"]
        .as_str()
        .unwrap()
        .contains("Accepted by Priya Kulkarni"));
    let case = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{pid}/onboarding"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let case_id = case["id"].as_str().unwrap();
    let cl = fetch(
        &app,
        &admin,
        &format!("{P}/onboarding-cases/{case_id}/checklist"),
    )
    .await;
    assert_eq!(cl["joiningDate"], join.to_string());
    assert_eq!(cl["progress"]["total"], 11, "{cl}");
    assert_eq!(cl["groups"][0]["category"], "Paperwork");
    let first = cl["groups"][0]["tasks"][0].clone();
    assert_eq!(
        first["dueDate"],
        (join - chrono::Duration::days(7)).to_string()
    );
    assert_eq!(first["owner"], "hr");
    let tid = first["id"].as_str().unwrap();
    let cl = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-tasks/{tid}/complete"),
        json!({"done": true}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(cl["progress"]["done"], 1);
    assert_eq!(cl["progress"]["pct"], 9);
    let cl = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{case_id}/checklist"),
        json!({"title": "Hostel warden allotment", "owner": "admin", "offsetDays": -1}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(cl["progress"]["total"], 12);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{case_id}/checklist"),
        json!({"title": "x", "owner": "canteen"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let due = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/onboarding-due"),
    )
    .await;
    assert!(
        due["items"].as_array().unwrap().is_empty(),
        "joining is far ahead: {due}"
    );

    // Analytics: one candidate, hired; acceptance 100 %.
    let an = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/hiring-analytics"),
    )
    .await;
    assert_eq!(an["candidates"], 1);
    assert_eq!(an["funnel"][4]["stage"], "hired");
    assert_eq!(an["funnel"][4]["count"], 1);
    assert_eq!(an["conversion"][0]["rate"], 1.0);
    assert_eq!(an["timeToHire"]["count"], 1);
    assert_eq!(an["offers"]["accepted"], 1);
    assert_eq!(an["offers"]["acceptance"], 1.0);
    assert_eq!(an["sources"][0]["source"], "Naukri");
    assert_eq!(an["requisitions"][0]["hired"], 1);
    let (s, _) = call(
        &app,
        &admin,
        "GET",
        &format!("{P}/institutions/{inst}/hiring-analytics?sinceDays=0"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // Decline and withdraw on a second requisition; settings round-trip.
    let req2 = seed_requisition(&pool, t, inst, "REQ-005002", 2).await;
    let rohan = seed_candidate(&pool, t, inst, req2, "Rohan", "interviewed").await;
    let meera = seed_candidate(&pool, t, inst, req2, "Meera", "shortlisted").await;
    let o2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{rohan}/offers"),
        json!({"joiningDate": "2027-08-01", "ctc": {"basic": "40000"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let o2id = o2["id"].as_str().unwrap().to_owned();
    let o2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{o2id}/submit"),
        json!({}),
        ver(&o2),
        StatusCode::OK,
    )
    .await;
    let o2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{o2id}/send"),
        json!({}),
        ver(&o2),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{o2id}/decline"),
        json!({}),
        ver(&o2),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let o2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{o2id}/decline"),
        json!({"reason": "Joined a university in Pune"}),
        ver(&o2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o2["status"], "declined");
    let c = fetch(&app, &admin, &format!("{P}/candidates/{rohan}")).await;
    assert_eq!(c["status"], "offer_declined");
    let o3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{meera}/offers"),
        json!({"joiningDate": "2027-08-01", "ctc": {"basic": "38000"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let o3id = o3["id"].as_str().unwrap().to_owned();
    let o3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/offers/{o3id}/withdraw"),
        json!({"reason": "Budget re-planned"}),
        ver(&o3),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o3["status"], "withdrawn");
    let list = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/offers?status=declined"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    let s = ok(&app, &admin, "PUT", &format!("{P}/institutions/{inst}/offer-settings"),
        json!({"offerTemplate": "Dear {{name}},\nCTC {{ctcAnnual}}\nBonus {{joiningBonus}}",
               "onboardingTemplates": [{"id": "lab", "name": "Lab staff", "jobIds": [req2.to_string()],
                   "tasks": [{"title": "Lab safety induction", "category": "Department", "offsetDays": 1, "owner": "department"}]},
                   {"id": "bad id", "name": "x", "tasks": [{"title": "t"}]}],
               "staleAfterDays": 7, "retentionMonths": 6}), None, StatusCode::OK).await;
    assert_eq!(s["onboardingTemplates"].as_array().unwrap().len(), 1);
    assert_eq!(s["staleAfterDays"], 7);
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/institutions/{inst}/offer-settings"),
        json!({"staleAfterDays": 0}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let p = fetch(&app, &admin, &format!("{P}/offers/{o3id}/letter")).await;
    assert_eq!(
        p["text"], "Dear Meera,\nCTC ₹4,77,600",
        "custom template, empty bonus line dropped"
    );
}

/// People router with a hand-built state and an injected administrator session.
fn custom_app(st: sutra_people::PeopleState, tenant: &str, subject: &str) -> Router {
    let (router, _) = sutra_people::router_with_state(st).split_for_parts();
    let ctx = AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![Role::Administrator],
        scope: ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap()),
        csrf_token: "x".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        idle_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        mfa_level: MfaLevel::Mfa,
        amr: vec!["pwd".into(), "otp".into()],
        step_up_enforced: false,
        privileged_mfa_enforced: false,
    };
    router.layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let ctx = ctx.clone();
            async move {
                req.extensions_mut().insert(ctx);
                next.run(req).await
            }
        },
    ))
}

#[tokio::test]
async fn offer_approval_needs_a_second_person() {
    let (app, pool) = app(TENANT_SOD).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_SOD, "Approvals College").await;
    let inst = setup.institution().as_uuid();
    let t = setup.tenant().as_uuid();
    let req = seed_requisition(&pool, t, inst, "REQ-005101", 1).await;
    let cand = seed_candidate(&pool, t, inst, req, "Kavya", "interviewed").await;
    let mut st = sutra_api::app::people_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let maker = custom_app(st.clone(), TENANT_SOD, "dev|hr.maker@sutra.dev");
    let checker = custom_app(st, TENANT_SOD, "dev|hr.checker@sutra.dev");
    let me = Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    };

    let o = ok(
        &maker,
        &me,
        "POST",
        &format!("{P}/candidates/{cand}/offers"),
        json!({"joiningDate": "2027-09-01", "ctc": {"basic": "45000", "hra": "9000"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let id = o["id"].as_str().unwrap().to_owned();
    let o = ok(
        &maker,
        &me,
        "POST",
        &format!("{P}/offers/{id}/submit"),
        json!({}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "pending_approval");
    assert_eq!(o["submittedBy"], "dev|hr.maker@sutra.dev");
    let (s, b) = call(
        &maker,
        &me,
        "POST",
        &format!("{P}/offers/{id}/decision"),
        json!({"decision": "approve"}),
        ver(&o),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "drafter cannot approve: {b}");
    let (s, _) = call(
        &checker,
        &me,
        "POST",
        &format!("{P}/offers/{id}/decision"),
        json!({"decision": "reject"}),
        ver(&o),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "send-back needs a reason"
    );
    let o = ok(
        &checker,
        &me,
        "POST",
        &format!("{P}/offers/{id}/decision"),
        json!({"decision": "reject", "reason": "HRA above policy"}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "draft");
    let o = ok(
        &maker,
        &me,
        "PUT",
        &format!("{P}/offers/{id}"),
        json!({"joiningDate": "2027-09-01", "ctc": {"basic": "45000", "hra": "4500"}}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    let o = ok(
        &maker,
        &me,
        "POST",
        &format!("{P}/offers/{id}/submit"),
        json!({}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    let o = ok(
        &checker,
        &me,
        "POST",
        &format!("{P}/offers/{id}/decision"),
        json!({"decision": "approve"}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "approved");
    assert_eq!(o["decidedBy"], "dev|hr.checker@sutra.dev");
    // The database refuses a self-approval even if a handler forgot the guard.
    let err =
        sqlx::query("UPDATE people.candidate_offer SET decided_by = drafted_by WHERE id = $1")
            .bind(Uuid::parse_str(&id).unwrap())
            .execute(&pool)
            .await;
    assert!(err.is_err(), "SoD CHECK");
}

#[tokio::test]
async fn public_careers_page_lists_and_applies() {
    let (app, pool) = app(TENANT_PUB).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_PUB, "Careers College").await;
    let inst = setup.institution().as_uuid();
    let t = setup.tenant().as_uuid();
    let req = seed_requisition(&pool, t, inst, "REQ-005201", 2).await;
    let slug = format!("careers-{}", &Uuid::now_v7().simple().to_string()[20..]);

    let page = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/careers-page"),
    )
    .await;
    assert_eq!(page["enabled"], false);
    assert!(page["suggestedSlug"]
        .as_str()
        .unwrap()
        .starts_with("careers-college"));
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/institutions/{inst}/careers-page"),
        json!({"enabled": true}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "slug required");
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/institutions/{inst}/careers-page"),
        json!({"enabled": true, "slug": "-Bad"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let page = ok(&app, &admin, "PUT", &format!("{P}/institutions/{inst}/careers-page"),
        json!({"enabled": true, "slug": slug, "intro": "Join a NAAC A+ college.\n- Campus housing", "contactEmail": "hr@careers.sutra.dev"}),
        None, StatusCode::OK).await;
    assert_eq!(page["publicPath"], format!("/careers/{slug}"));
    let page = ok(&app, &admin, "PUT", &format!("{P}/requisitions/{req}/careers-listing"),
        json!({"published": true, "description": "Teach UG commerce.\n\n- NET/SET cleared\n- <b>PhD</b> preferred", "location": "Ahilyanagar"}),
        None, StatusCode::OK).await;
    assert_eq!(page["listings"][0]["published"], true);

    // Public list: no session, safe fields, escaped description.
    let public = format!("{P}/public/careers/{slug}");
    let (s, _, b) = send(
        &app,
        Request::get(public.as_str()).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["institution"], "Careers College");
    let opening = &b["openings"][0];
    assert_eq!(opening["id"], req.to_string());
    assert!(opening["descriptionHtml"]
        .as_str()
        .unwrap()
        .contains("<li>&lt;b&gt;PhD&lt;/b&gt; preferred</li>"));
    assert!(opening.get("justification").is_none() && opening.get("institutionId").is_none());
    let (s, _, _) = send(
        &app,
        Request::get(format!("{P}/public/careers/no-such-page"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    let apply = |body: String| {
        Request::post(format!("{public}/requisitions/{req}/apply"))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    };
    let (s, _, b) = send(
        &app,
        apply(
            json!({"name": "Anita Patil", "email": "Anita@Example.in", "phone": "+91 98220 12345",
        "note": "Eight years of teaching", "link": "https://example.in/cv.pdf"})
            .to_string(),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED, "{b}");
    // Duplicate and honeypot: same answer, nothing stored.
    let (s, _, _) = send(
        &app,
        apply(json!({"name": "Anita Patil", "email": "anita@example.in"}).to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (s, _, _) = send(
        &app,
        apply(json!({"name": "Bot", "email": "bot@spam.in", "website": "http://spam"}).to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    let (s, _, _) = send(
        &app,
        apply(json!({"name": "", "email": "nope"}).to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = send(
        &app,
        apply(
            json!({"name": "Big", "email": "big@example.in", "note": "x".repeat(20_000)})
                .to_string(),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE);
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM people.candidate WHERE requisition_id = $1")
            .bind(req)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
    let (given, family, source, status): (String, Option<String>, Option<String>, String) = sqlx::query_as(
        "SELECT given_name, family_name, source, status FROM people.candidate WHERE requisition_id = $1",
    )
    .bind(req)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (
            given.as_str(),
            family.as_deref(),
            source.as_deref(),
            status.as_str()
        ),
        ("Anita", Some("Patil"), Some("Careers page"), "applied")
    );

    // Unpublished or closed requisitions disappear and refuse applications.
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/requisitions/{req}/careers-listing"),
        json!({"published": false}),
        None,
        StatusCode::OK,
    )
    .await;
    let (_, _, b) = send(
        &app,
        Request::get(public.as_str()).body(Body::empty()).unwrap(),
    )
    .await;
    assert!(b["openings"].as_array().unwrap().is_empty());
    let (s, _, _) = send(
        &app,
        apply(json!({"name": "Late Comer", "email": "late@example.in"}).to_string()),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Disabled page is gone.
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/institutions/{inst}/careers-page"),
        json!({"enabled": false, "slug": slug}),
        None,
        StatusCode::OK,
    )
    .await;
    let (s, _, _) = send(
        &app,
        Request::get(public.as_str()).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
