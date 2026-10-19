//! ERP_EXTENSIONS 3B — hiring scorecards over the HTTP API against PostgreSQL: rubric and
//! guides, requisition hiring form, screening answers and must-have flags, interviews (guide
//! picked by format), scorecards (validation, rubric snapshot), panel summary, comparison,
//! feedback due, cancel.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use chrono::Duration;
use serde_json::{json, Value};
use support::b7;
use support::*;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000e3b00";
const P: &str = "/api/v1/people";

async fn ok(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, None)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &axum::Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

async fn seed_candidate(pool: &sqlx::PgPool, t: Uuid, inst: Uuid, req: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO people.candidate (tenant_id, institution_id, requisition_id, given_name, email, status) \
         VALUES ($1, $2, $3, $4, $5, 'shortlisted') RETURNING id",
    )
    .bind(t)
    .bind(inst)
    .bind(req)
    .bind(name)
    .bind(format!("{}@applicants.sutra.dev", name.to_lowercase()))
    .fetch_one(pool)
    .await
    .expect("candidate")
}

#[tokio::test]
async fn hiring_scorecards_rubric_interviews_feedback() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Scorecard College").await;
    let inst = setup.institution().as_uuid();
    let t = setup.tenant().as_uuid();
    let req: Uuid = sqlx::query_scalar(
        "INSERT INTO people.requisition (tenant_id, institution_id, requisition_number, title, staff_category, \
         employment_type, designation, openings, status) VALUES ($1, $2, 'REQ-000901', 'Assistant Professor, \
         Commerce', 'teaching', 'permanent', 'Assistant Professor', 1, 'open') RETURNING id",
    )
    .bind(t)
    .bind(inst)
    .fetch_one(&pool)
    .await
    .expect("requisition");
    let priya = seed_candidate(&pool, t, inst, req, "Priya").await;
    let rohan = seed_candidate(&pool, t, inst, req, "Rohan").await;

    // Institution settings: default rubric falls back; guides are cleaned.
    let s = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/hiring-settings"),
    )
    .await;
    assert!(s["ratingFields"].is_null());
    let s = ok(&app, &admin, "PUT", &format!("{P}/institutions/{inst}/hiring-settings"),
        json!({"ratingFields": null, "guides": [
            {"id": "demo", "name": "Teaching demo", "types": ["onsite"], "stages": [],
             "questions": [{"id": "q1", "text": "Teach a 10-minute lesson on GST input credit", "field": "teaching_demo"}]},
            {"id": "bad id", "name": "Dropped", "questions": [{"id": "q", "text": "x"}]}
        ]}), StatusCode::OK).await;
    assert_eq!(s["guides"].as_array().unwrap().len(), 1);

    // Requisition rubric + screening; omitted keys are kept.
    let f = ok(&app, &admin, "PUT", &format!("{P}/requisitions/{req}/hiring-form"),
        json!({"ratingFields": [{"key": "subject_knowledge", "label": "Subject knowledge"},
                                {"key": "teaching_demo", "label": "Teaching demonstration"}],
               "screeningQuestions": [{"id": "net", "label": "Cleared NET/SET?", "kind": "yesno", "mustHave": true}]}),
        StatusCode::OK).await;
    assert_eq!(f["rubric"].as_array().unwrap().len(), 2);
    let f = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/requisitions/{req}/hiring-form"),
        json!({"ratingFields": [{"key": "subject_knowledge", "label": "Subject knowledge"},
                                {"key": "teaching_demo", "label": "Teaching demonstration"},
                                {"key": "research", "label": "Research"}]}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["screeningQuestions"].as_array().unwrap().len(), 1, "{f}");
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/candidates/{rohan}/screening-answers"),
        json!({"answers": [{"label": "cleared net/set?", "answer": "Not yet"}]}),
        StatusCode::NO_CONTENT,
    )
    .await;

    // Interviews: onsite picks the demo guide; one held 3 days ago, one yesterday.
    let today = chrono::Utc::now().date_naive();
    let old = ok(&app, &admin, "POST", &format!("{P}/candidates/{priya}/interviews"),
        json!({"type": "onsite", "date": today - Duration::days(3), "start": "10:00", "end": "11:00",
               "interviewers": ["Dr. Rao", " "]}), StatusCode::CREATED).await;
    assert_eq!(old["guide"]["id"], "demo");
    assert_eq!(old["interviewers"], json!(["Dr. Rao"]));
    let rohan_itv = ok(&app, &admin, "POST", &format!("{P}/candidates/{rohan}/interviews"),
        json!({"type": "phone", "date": today - Duration::days(4), "start": "15:00", "end": "15:30"}), StatusCode::CREATED).await;
    assert!(rohan_itv["guide"].is_null());
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{priya}/interviews"),
        json!({"type": "video", "date": today, "start": "11:00", "end": "10:00"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // Feedback due lists both (dates depend on the institution's day; allow either).
    let due = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/feedback-due"),
    )
    .await;
    assert_eq!(due["items"].as_array().unwrap().len(), 2, "{due}");

    // Scorecards: validation and unknown rubric keys refused; good ones saved with the rubric.
    let itv = old["id"].as_str().unwrap();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{priya}/scorecards"),
        json!({"authorName": " ", "recommendation": "yes"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{priya}/scorecards"),
        json!({"authorName": "Dr. Rao", "recommendation": "yes", "ratings": {"skills": 4}}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{priya}/scorecards"),
        json!({"authorName": "Dr. Rao", "recommendation": "yes", "ratings": {"research": 6}}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c1 = ok(&app, &admin, "POST", &format!("{P}/candidates/{priya}/scorecards"),
        json!({"interviewId": itv, "authorName": "Dr. Rao", "recommendation": "strong_yes",
               "ratings": {"subject_knowledge": 5, "teaching_demo": 4, "research": null},
               "guideAnswers": [{"question": "Teach a 10-minute lesson on GST input credit", "answer": "Clear, used examples", "field": "teaching_demo"}]}),
        StatusCode::CREATED).await;
    assert_eq!(c1["average"], 4.5);
    assert_eq!(c1["fields"].as_array().unwrap().len(), 3);
    ok(&app, &admin, "POST", &format!("{P}/candidates/{priya}/scorecards"),
        json!({"authorName": "Prof. Iyer", "recommendation": "no", "ratings": {"subject_knowledge": 3}}), StatusCode::CREATED).await;

    let fb = fetch(&app, &admin, &format!("{P}/candidates/{priya}/feedback")).await;
    assert_eq!(fb["summary"]["count"], 2);
    assert_eq!(fb["summary"]["leaning"], "hire");
    assert_eq!(fb["summary"]["byField"]["subject_knowledge"], 4.0);
    assert!(fb["summary"]["byField"]["research"].is_null());
    assert_eq!(fb["interviews"][0]["hasScorecard"], true);
    let fr = fetch(&app, &admin, &format!("{P}/candidates/{rohan}/feedback")).await;
    assert_eq!(fr["screening"]["mismatches"], json!(["net"]));

    let cmp = fetch(&app, &admin, &format!("{P}/requisitions/{req}/scorecards")).await;
    assert_eq!(cmp["items"][0]["candidateId"], priya.to_string());
    assert_eq!(cmp["items"][1]["screeningFlags"], 1);
    assert!(cmp["items"][1]["summary"]["leaning"].is_null());

    // Priya is covered; cancelling Rohan's interview clears the list.
    let due = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/feedback-due"),
    )
    .await;
    assert_eq!(due["items"].as_array().unwrap().len(), 1, "{due}");
    let rid = rohan_itv["id"].as_str().unwrap();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidate-interviews/{rid}/cancel"),
        json!({}),
        StatusCode::NO_CONTENT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidate-interviews/{rid}/cancel"),
        json!({}),
        StatusCode::CONFLICT,
    )
    .await;
    let due = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/feedback-due"),
    )
    .await;
    assert!(due["items"].as_array().unwrap().is_empty(), "{due}");
}
