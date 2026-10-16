//! Batch 6 slice 6C (Results) DB tests — seeded through `support::b6` (no dependency on 6A / 6B
//! APIs):
//!
//! * `results_engine_publication_and_grade_cards` — L3-I-083 (computation → SGPA / CGPA →
//!   verification → approval → publication), 085 (non-final marks → withheld), 086 / 098
//!   (resolution → release), 087 / 093 (recompute, idempotent re-run, one live batch), 097 / 102
//!   (withhold an approved result), 101 / 104 / 105 / 116 (publication: delay + resume,
//!   cancellation, failure → safe retry), 107 / 117 (post-publication correction →
//!   `term_result.revised`), 109 / 110 (withdrawal / restoration), grade cards issue / duplicate /
//!   reissue, student view (nothing before publication, withdrawn = notice only), `ResultsPort`
//!   returns the published result to Domain C.
//! * `revaluation_supplementary_completion_and_closure` — L3-I-118 (revaluation with the Finance
//!   fee through `ExamFeePort`, settlement through `ChargeDirectory`, revised marks via
//!   `revise_final`, result revision), 120 / 121 (withdrawal before / not after review start),
//!   128 / 130 (duplicate / conflicting requests), 132 (finalised correction), 133 / 134 / 138 /
//!   139 (supplementary eligibility, authorisation, duplicates, best result), 145 / 150 / 153
//!   (completion evaluation + evidence, re-evaluation), 187 / 192 (closure blocked → closed →
//!   archived, locked registrations → completed).
//! * `await_decision_keeps_governed_steps_pending` — `ApprovalMode::AwaitDecision` for result
//!   approval (reject + approve), publication authorisation, correction and session closure
//!   (reject reopens, approve closes).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b6::{self, dec};
use support::*;
use sutra_contracts::exams::status::{ExamRegistrationStatus, ResultBatchStatus, ResultStatus};
use sutra_contracts::exams::StudentTermResultQuery;
use sutra_contracts::finance::ids::ChargeId;
use sutra_exams::results::repo as rr;
use sutra_exams::shared::approval::ApprovalMode;
use sutra_exams::shared::Actor;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b6c00";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000b6c01";
const TENANT_C: &str = "0192f5a0-7c3e-7000-8000-0000000b6c02";
const B: &str = "/api/v1/exams";

async fn call(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().expect("version")
}

fn id_of(v: &Value) -> String {
    v["id"].as_str().expect("id").to_owned()
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

/// The single current result of the session (staff view).
async fn current(app: &axum::Router, admin: &Login, session: Uuid) -> Value {
    let (s, list) = read(app, admin, &format!("{B}/exam-sessions/{session}/results")).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    list["items"][0].clone()
}

#[tokio::test]
async fn results_engine_publication_and_grade_cards() {
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT_A,
        "Results University",
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    let sid = setup.session_id.as_uuid();
    let md = b4::seed_offering(&pool, &setup.finance.delivery, "MD101", "active").await;
    let paper2 = b6::seed_paper(
        &pool,
        &setup.finance,
        setup.session_id,
        &md,
        "3",
        "40",
        "conducted",
    )
    .await;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id, paper2],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let rp = |p| {
        reg.papers
            .iter()
            .find(|x| x.paper_id == p)
            .unwrap()
            .registration_paper_id
    };
    b6::seed_final_marks(
        &pool,
        &setup,
        rp(setup.paper_id),
        Some(("30", "40")),
        Some("60"),
    )
    .await;

    // ---- one live batch per session; faculty cannot compute.
    let batches = format!("{B}/exam-sessions/{sid}/result-batches");
    let (s, batch) = call(&app, &admin, "POST", &batches, json!({}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{batch}");
    assert_eq!(batch["status"], "pending");
    let bid = id_of(&batch);
    let (s, _) = call(&app, &admin, "POST", &batches, json!({}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "L3-I-093: one live batch");
    let compute = format!("{B}/result-batches/{bid}/compute");
    let (s, _) = call(&app, &meera, "POST", &compute, json!({}), Some(ver(&batch))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&app, &admin, "POST", &compute, json!({}), None).await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);

    // ---- L3-I-085: a non-final mark withholds the result.
    let (s, run) = call(&app, &admin, "POST", &compute, json!({}), Some(ver(&batch))).await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(
        (run["processed"].as_i64(), run["withheld"].as_i64()),
        (Some(1), Some(1))
    );
    assert_eq!(run["batch"]["status"], "computed");
    let r1 = current(&app, &admin, sid).await;
    assert_eq!(
        (r1["status"].as_str(), r1["outcome"].as_str()),
        (Some("withheld"), Some("withheld"))
    );
    assert!(
        r1["statusReason"].as_str().unwrap().contains("not final"),
        "{r1}"
    );
    assert!(r1["lines"].as_array().unwrap().is_empty());
    assert_eq!(
        outbox(&pool, "sutraerp.exams.term_result.withheld", &id_of(&r1)).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/result-batches/{bid}/validate"),
        json!({}),
        Some(ver(&run["batch"])),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "only withheld results: nothing to validate"
    );
    let (s, mine) = read(&app, &aarav, &format!("{B}/me/results")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        mine["items"].as_array().unwrap().is_empty(),
        "nothing before publication"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{}/release", id_of(&r1)),
        json!({}),
        Some(ver(&r1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "still blocked");

    // ---- L3-I-086 / 098: resolution → release → computed revision with SGPA / CGPA.
    b6::seed_final_marks(&pool, &setup, rp(paper2), Some(("36", "40")), Some("85")).await;
    let (s, r2) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{}/release", id_of(&r1)),
        json!({"reason": "marks finalised"}),
        Some(ver(&r1)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r2}");
    assert_eq!(r2["status"], "computed");
    assert_eq!(r2["revision"], 2);
    assert_eq!(r2["supersedesId"], r1["id"]);
    // 30/40×40 + 60/100×60 = 66 → B+ (7); 36/40×40 + 85/100×60 = 87 → A+ (9);
    // SGPA = (4×7 + 3×9) / 7 = 7.857… → 7.86.
    assert_eq!(r2["sgpa"], "7.86");
    assert_eq!(r2["cgpa"], "7.86");
    assert_eq!(r2["creditsAttempted"], "7.00");
    assert_eq!(r2["creditsEarned"], "7.00");
    assert_eq!(r2["gradePoints"], "55.00");
    assert_eq!(r2["outcome"], "pass");
    assert_eq!(r2["lines"][0]["percent"], "66.00");
    assert_eq!(r2["lines"][0]["grade"], "B+");
    assert_eq!(r2["lines"][1]["percent"], "87.00");
    assert_eq!(r2["lines"][1]["grade"], "A+");

    // ---- L3-I-087 / 093: recompute before approval is idempotent (no new revision).
    let (_, b) = read(&app, &admin, &format!("{B}/result-batches/{bid}")).await;
    let (s, run) = call(&app, &admin, "POST", &compute, json!({}), Some(ver(&b))).await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(
        (run["unchanged"].as_i64(), run["computed"].as_i64()),
        (Some(1), Some(0))
    );
    assert_eq!(current(&app, &admin, sid).await["revision"], 2);

    // ---- L3-I-083: verification + approval (auto-approved in Batch 6).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/result-batches/{bid}/validate"),
        json!({"notes": "checked against mark sheets"}),
        Some(ver(&run["batch"])),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "approved");
    assert!(b["processInstanceId"].is_string());
    assert_eq!(
        outbox(&pool, "sutraerp.exams.result_batch.approved", &bid).await,
        1
    );
    let r = current(&app, &admin, sid).await;
    assert_eq!(r["status"], "approved");

    // ---- L3-I-097 / 102: withhold an approved result, then release it back to approved.
    let rid = id_of(&r);
    let (s, w) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid}/withhold"),
        json!({}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required: {w}");
    let (s, w) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid}/withhold"),
        json!({"reason": "identity check pending"}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{w}");
    assert_eq!(w["status"], "withheld");
    let (s, rel) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid}/release"),
        json!({}),
        Some(ver(&w)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rel}");
    assert_eq!(
        (rel["status"].as_str(), rel["revision"].as_i64()),
        (Some("approved"), Some(2))
    );

    // ---- finalise → publication: authorise, delay / resume, failure → retry (L3-I-104/116).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/result-batches/{bid}/finalise"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "ready_for_publication");
    let pubs = format!("{B}/exam-sessions/{sid}/publications");
    let (s, p) = call(&app, &admin, "POST", &pubs, json!({}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(p["status"], "planned");
    let pid = id_of(&p);
    let (s, _) = call(&app, &admin, "POST", &pubs, json!({}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "one open publication per session");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/publish"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "publication needs authorisation");
    let (s, p) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/authorise"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["status"], "authorised");
    let (s, p) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/delay"),
        json!({"reason": "board meeting moved"}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["status"], "delayed");
    assert_eq!(
        outbox(&pool, "sutraerp.exams.publication.delayed", &pid).await,
        1
    );
    let (s, p) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/resume"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["status"], "authorised");
    // A publication run that failed (e.g. a dependency outage) is left `failed`; retry recovers.
    sqlx::query("UPDATE exams.publication SET status = 'failed', last_error = 'simulated outage' WHERE id = $1")
        .bind(Uuid::parse_str(&pid).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (_, p) = read(&app, &admin, &format!("{B}/publications/{pid}")).await;
    assert_eq!(p["status"], "failed");
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/retry"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(run["published"], 1);
    assert_eq!(run["publication"]["status"], "published");
    let r = current(&app, &admin, sid).await;
    assert_eq!(r["status"], "published");
    assert!(r["publishedAt"].is_string());
    assert_eq!(
        outbox(&pool, "sutraerp.exams.term_result.published", &rid).await,
        1
    );
    let (_, b) = read(&app, &admin, &format!("{B}/result-batches/{bid}")).await;
    assert_eq!(b["status"], "closed");

    // ---- Domain C reads the published result through ResultsPort.
    let results = sutra_api::app::student_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .results;
    let query = StudentTermResultQuery {
        institution_id: setup.institution(),
        student_id: setup.student_id(),
        academic_term_id: setup.term_id(),
    };
    let summary = results
        .term_result(&setup.scope(), query.clone())
        .await
        .unwrap()
        .expect("published result visible to Domain C");
    assert!((summary.sgpa.unwrap() - 7.86).abs() < 1e-9, "{summary:?}");
    assert_eq!(summary.revision, 2);

    // ---- student / staff views.
    let (s, mine) = read(&app, &aarav, &format!("{B}/me/results")).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine["items"][0]["sgpa"], "7.86");
    assert_eq!(mine["items"][0]["lines"].as_array().unwrap().len(), 2);
    let (s, _) = read(&app, &aarav, &format!("{B}/exam-sessions/{sid}/results")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students have no staff read");
    // Part 1 S3 (DEF-I-006): exam.read no longer exposes results to faculty (examination office only).
    let (s, _) = read(&app, &meera, &format!("{B}/exam-sessions/{sid}/results")).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "faculty must not read results (DEF-I-006)"
    );

    // ---- grade cards: issue, duplicate, self view.
    let (s, card) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid}/grade-card"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{card}");
    assert!(card["number"].as_str().unwrap().starts_with("GC-"));
    assert_eq!(card["status"], "issued");
    assert_eq!(card["result"]["sgpa"], "7.86");
    let card_id = id_of(&card);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid}/grade-card"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one valid card per result");
    let (s, cards) = read(&app, &aarav, &format!("{B}/me/grade-cards")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cards["items"].as_array().unwrap().len(), 1);

    // ---- L3-I-107 / 117: approved post-publication correction → revised result.
    let (s, out) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid}/correct"),
        json!({"paperId": setup.paper_id, "externalMarks": "70", "reason": "totalling error"}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["decision"]["status"], "approved");
    let r3 = &out["result"];
    assert_eq!(
        (r3["revision"].as_i64(), r3["status"].as_str()),
        (Some(3), Some("published"))
    );
    assert_eq!(r3["supersedesId"].as_str(), Some(rid.as_str()));
    // 30 + 70/100×60 = 72 → A (8): SGPA = (32 + 27) / 7 = 8.43.
    assert_eq!(r3["sgpa"], "8.43");
    let rid3 = id_of(r3);
    assert_eq!(
        outbox(&pool, "sutraerp.exams.term_result.revised", &rid3).await,
        1
    );
    let (_, old) = read(&app, &admin, &format!("{B}/term-results/{rid}")).await;
    assert_eq!(old["status"], "superseded");
    let (_, card) = read(&app, &admin, &format!("{B}/grade-cards/{card_id}")).await;
    assert_eq!(
        card["status"], "withdrawn",
        "a revision withdraws the old card"
    );
    let (s, card2) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/grade-cards/{card_id}/reissue"),
        json!({"reason": "revised result"}),
        Some(ver(&card)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{card2}");
    assert_eq!(card2["status"], "reissued");
    assert_eq!(card2["resultId"].as_str(), Some(rid3.as_str()));
    assert_eq!(card2["supersedesId"].as_str(), Some(card_id.as_str()));

    // ---- L3-I-109 / 110: withdrawal (student sees a notice) and restoration.
    let (s, out) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid3}/withdraw"),
        json!({"reason": "under inquiry"}),
        Some(ver(r3)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["decision"]["kind"], "withdrawal");
    assert_eq!(out["result"]["status"], "withdrawn");
    assert_eq!(
        outbox(&pool, "sutraerp.exams.term_result.withdrawn", &rid3).await,
        1
    );
    let (_, mine) = read(&app, &aarav, &format!("{B}/me/results")).await;
    let shown = &mine["items"][0];
    assert_eq!(shown["status"], "withdrawn");
    assert!(shown["lines"].as_array().unwrap().is_empty());
    assert!(shown["notice"].is_string() && shown["sgpa"].is_null());
    let (_, card2) = read(&app, &admin, &format!("{B}/grade-cards/{}", id_of(&card2))).await;
    assert_eq!(card2["status"], "withdrawn");
    let (s, restored) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/term-results/{rid3}/restore"),
        json!({}),
        Some(ver(&out["result"])),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{restored}");
    assert_eq!(restored["status"], "published");

    // ---- L3-I-105: a pending publication can be cancelled.
    let (s, p2) = call(&app, &admin, "POST", &pubs, json!({}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{p2}");
    let (s, p2) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{}/cancel", id_of(&p2)),
        json!({"reason": "nothing new to publish"}),
        Some(ver(&p2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p2}");
    assert_eq!(p2["status"], "cancelled");
}

#[tokio::test]
async fn revaluation_supplementary_completion_and_closure() {
    let (app, pool) = app(TENANT_B).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT_B,
        "Revaluation University",
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    let sid = setup.session_id.as_uuid();
    let paper = setup.paper_id.as_uuid();
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Locked,
    )
    .await;
    let mark = b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("20", "40")),
        Some("30"),
    )
    .await;

    // ---- compute → validate (auto-approved) → finalise → publish: 20 + 18 = 38 % → F.
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/exam-sessions/{sid}/result-batches"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    let bid = id_of(&b);
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/result-batches/{bid}/compute"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/result-batches/{bid}/validate"),
        json!({}),
        Some(ver(&run["batch"])),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/result-batches/{bid}/finalise"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, p) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/exam-sessions/{sid}/publications"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    let pid = id_of(&p);
    let (s, p) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/authorise"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/publications/{pid}/publish"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    let r = current(&app, &admin, sid).await;
    assert_eq!(
        (r["status"].as_str(), r["outcome"].as_str()),
        (Some("published"), Some("fail"))
    );
    assert_eq!(r["lines"][0]["percent"], "38.00");
    assert_eq!(r["lines"][0]["grade"], "F");

    // ---- L3-I-133 / 138 / 139 / 134: supplementary eligibility.
    let elig = format!("{B}/exam-sessions/{sid}/supplementary-eligibility");
    let (s, sum) = call(&app, &admin, "POST", &elig, json!({}), None).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(sum["eligible"], 1);
    let (s, sum) = call(&app, &admin, "POST", &elig, json!({}), None).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(
        (sum["eligible"].as_i64(), sum["skipped"].as_i64()),
        (Some(0), Some(1))
    );
    let (_, list) = read(&app, &admin, &elig).await;
    let e = &list["items"][0];
    assert_eq!(e["status"], "eligible");
    assert_eq!(
        (e["attemptsUsed"].as_i64(), e["maxAttempts"].as_i64()),
        (Some(1), Some(4))
    );
    assert_eq!(
        (e["bestPercent"].as_str(), e["bestGrade"].as_str()),
        (Some("38.00"), Some("F"))
    );
    let authorise = format!("{B}/supplementary-eligibility/{}/authorise", id_of(e));
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &authorise,
        json!({"supplementarySessionId": sid}),
        Some(ver(e)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a regular session offers no attempt"
    );
    let supp = b6::seed_session(&pool, &setup.finance, setup.scheme_id, "scheduled").await;
    sqlx::query("UPDATE exams.exam_session SET kind = 'supplementary' WHERE id = $1")
        .bind(supp.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    let (s, e2) = call(
        &app,
        &admin,
        "POST",
        &authorise,
        json!({"supplementarySessionId": supp}),
        Some(ver(e)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{e2}");
    assert_eq!(e2["status"], "authorised");

    // ---- L3-I-118: revaluation with the Finance fee.
    let mine = format!("{B}/me/revaluation-requests");
    let (s, rev) = call(
        &app,
        &aarav,
        "POST",
        &mine,
        json!({"paperId": paper, "kind": "revaluation", "reason": "expected a higher score"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rev}");
    assert_eq!(rev["status"], "fee_pending");
    assert!(rev["number"].as_str().unwrap().starts_with("REV-"));
    assert_eq!(rev["feeAmount"], "500.00");
    assert!(rev["feeChargeNumber"].as_str().unwrap().starts_with("CHG-"));
    assert_eq!(rev["feeSettled"], false);
    let rev_id = id_of(&rev);
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &mine,
        json!({"paperId": paper, "kind": "revaluation"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "L3-I-128 duplicate");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &mine,
        json!({"paperId": paper, "kind": "rechecking"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "L3-I-130 conflicting kinds");
    let (s, staff) = read(
        &app,
        &admin,
        &format!("{B}/exam-sessions/{sid}/revaluation-requests"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(staff["items"].as_array().unwrap().len(), 1);
    let rr_path = |verb: &str| format!("{B}/revaluation-requests/{rev_id}/{verb}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &rr_path("verify"),
        json!({}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "fee outstanding");
    let charge = Uuid::parse_str(rev["feeChargeId"].as_str().unwrap()).unwrap();
    b5::seed_payment(&pool, &setup.finance, ChargeId(charge), "500.00").await;
    let (s, rev) = call(
        &app,
        &admin,
        "POST",
        &rr_path("verify"),
        json!({}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rev}");
    assert_eq!(rev["status"], "approved");
    assert_eq!(rev["feeSettled"], true);
    assert_eq!(
        outbox(&pool, "sutraerp.exams.revaluation_request.decided", &rev_id).await,
        1
    );
    let (s, rev) = call(
        &app,
        &admin,
        "POST",
        &rr_path("start-review"),
        json!({}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rev}");
    assert_eq!(rev["status"], "review_in_progress");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{B}/me/revaluation-requests/{rev_id}/withdraw"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "L3-I-121: no withdrawal once the review started"
    );
    let (s, rev) = call(
        &app,
        &admin,
        "POST",
        &rr_path("complete"),
        json!({"externalMarks": "45", "notes": "question 3 re-marked"}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rev}");
    assert_eq!(rev["status"], "review_completed");
    assert_eq!(rev["outcome"]["fromMarks"], "30.00");
    assert_eq!(rev["outcome"]["toMarks"], "45.00");
    assert_eq!(rev["outcome"]["changed"], true);
    let (s, rev) = call(
        &app,
        &admin,
        "POST",
        &rr_path("verify-outcome"),
        json!({}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rev}");
    let (s, rev) = call(
        &app,
        &admin,
        "POST",
        &rr_path("approve-marks"),
        json!({}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rev}");
    assert_eq!(rev["status"], "closed");
    assert!(rev["revisedResultId"].is_string());
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.exams.revaluation_request.completed",
            &rev_id
        )
        .await,
        1
    );
    // 20 + 45/100×60 = 47 → P (4): the revised result is published as revision 2.
    let r2 = current(&app, &admin, sid).await;
    assert_eq!(
        (r2["revision"].as_i64(), r2["status"].as_str()),
        (Some(2), Some("published"))
    );
    assert_eq!(
        (r2["outcome"].as_str(), r2["sgpa"].as_str()),
        (Some("pass"), Some("4.00"))
    );
    assert_eq!(rev["revisedResultId"], r2["id"]);
    assert_eq!(
        outbox(&pool, "sutraerp.exams.term_result.revised", &id_of(&r2)).await,
        1
    );
    let revised: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.paper_mark_history WHERE paper_mark_id = $1 AND cause = 'revaluation'",
    )
    .bind(mark.id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(revised, 1, "final marks revised through revise_final");

    // ---- L3-I-132: correction of the finalised outcome.
    let (s, rev) = call(
        &app,
        &admin,
        "POST",
        &rr_path("correct"),
        json!({"externalMarks": "50", "reason": "re-totalling after review"}),
        Some(ver(&rev)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rev}");
    assert_eq!(
        (rev["status"].as_str(), rev["outcome"]["toMarks"].as_str()),
        (Some("closed"), Some("50.00"))
    );
    let r3 = current(&app, &admin, sid).await;
    assert_eq!(
        (r3["revision"].as_i64(), r3["sgpa"].as_str()),
        (Some(3), Some("5.00"))
    );

    // ---- L3-I-145 / 150 / 153: completion evaluation + evidence, re-evaluation.
    let inst = setup.institution().as_uuid();
    let (s, ce) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/institutions/{inst}/completion-evaluations"),
        json!({"studentId": setup.student_id()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ce}");
    assert_eq!(ce["creditsEarned"], "4.00");
    assert_eq!(ce["evidence"]["terms"].as_array().unwrap().len(), 1);
    assert_ne!(
        ce["status"], "eligible",
        "one paper does not complete the programme"
    );
    let (s, ce2) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/completion-evaluations/{}/reevaluate", id_of(&ce)),
        json!({}),
        Some(ver(&ce)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ce2}");
    assert_eq!(ce2["supersedesId"], ce["id"]);
    let (_, hist) = read(
        &app,
        &admin,
        &format!(
            "{B}/institutions/{inst}/completion-evaluations?studentId={}&includeSuperseded=true",
            setup.student_id()
        ),
    )
    .await;
    assert_eq!(hist["items"].as_array().unwrap().len(), 2, "{hist}");

    // ---- L3-I-120: a rechecking withdrawn before its review.
    let (s, rc) = call(
        &app,
        &aarav,
        "POST",
        &mine,
        json!({"paperId": paper, "kind": "rechecking"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rc}");
    assert_eq!(rc["feeAmount"], "200.00");

    // ---- L3-I-187 / 192: closure blocked while a review is live, then closed and archived.
    let closure = format!("{B}/exam-sessions/{sid}/closure");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &format!("{closure}/initiate"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "closing");
    assert_eq!(c["checks"]["liveReviews"], 1);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{closure}/close"),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a live review blocks closure");
    let (s, rc) = call(
        &app,
        &aarav,
        "POST",
        &format!("{B}/me/revaluation-requests/{}/withdraw", id_of(&rc)),
        json!({"reason": "changed my mind"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rc}");
    assert_eq!(rc["status"], "withdrawn");
    let (_, c) = read(&app, &admin, &closure).await;
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &format!("{closure}/close"),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "closed");
    assert_eq!(c["completedRegistrations"], 1);
    let reg_status: String =
        sqlx::query_scalar("SELECT status FROM exams.exam_registration WHERE id = $1")
            .bind(reg.registration_id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(reg_status, "completed");
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.exams.exam_session.closed",
            &sid.to_string()
        )
        .await,
        1
    );
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &format!("{closure}/archive"),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "archived");
    let session_status: String =
        sqlx::query_scalar("SELECT status FROM exams.exam_session WHERE id = $1")
            .bind(sid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(session_status, "archived");
}

#[tokio::test]
async fn await_decision_keeps_governed_steps_pending() {
    let (app, pool) = app(TENANT_C).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT_C,
        "Decision University",
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Locked,
    )
    .await;
    b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("30", "40")),
        Some("60"),
    )
    .await;
    let mut st = sutra_api::app::exams_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let t = setup.tenant();
    let sid = setup.session_id;
    let maker = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let approver = Actor {
        id: Some("dev|approver@sutra.dev".into()),
        correlation_id: None,
    };

    // ---- result approval: validation leaves the batch pending; reject, then approve.
    let b = rr::create_batch(&st, t, sid, &maker).await.unwrap();
    let (b, sum) = rr::compute_batch(&st, t, b.id, b.version, &maker)
        .await
        .unwrap();
    assert_eq!(sum.computed, 1);
    let b = rr::validate_batch(&st, t, b.id, b.version, None, &maker)
        .await
        .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Validated);
    assert!(b.process_instance_id.is_some());
    let status = |rows: Vec<sutra_exams::shared::results::ResultRow>| rows[0].status;
    assert_eq!(
        status(rr::list_results(&st, t, sid, None, false).await.unwrap()),
        ResultStatus::Validated
    );
    let b = rr::decide_batch(
        &st,
        t,
        b.id,
        b.version,
        false,
        Some("recheck".into()),
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Computed);
    assert_eq!(
        status(rr::list_results(&st, t, sid, None, false).await.unwrap()),
        ResultStatus::Computed
    );
    let b = rr::validate_batch(&st, t, b.id, b.version, None, &maker)
        .await
        .unwrap();
    let b = rr::decide_batch(&st, t, b.id, b.version, true, None, &approver)
        .await
        .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Approved);
    assert_eq!(
        status(rr::list_results(&st, t, sid, None, false).await.unwrap()),
        ResultStatus::Approved
    );
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.exams.result_batch.approved",
            &b.id.to_string()
        )
        .await,
        1
    );
    let b = rr::finalise_batch(&st, t, b.id, b.version, &maker)
        .await
        .unwrap();
    assert_eq!(b.status, ResultBatchStatus::ReadyForPublication);

    // ---- publication authorisation awaits its decision.
    let p = rr::create_publication(&st, t, sid, None, &maker)
        .await
        .unwrap();
    let p = rr::authorise_publication(&st, t, p.id, p.version, &maker)
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "planned");
    assert!(p.process_instance_id.is_some());
    let early = rr::publish_publication(&st, t, p.id, p.version, &maker).await;
    assert!(matches!(early, Err(KernelError::Conflict(_))), "{early:?}");
    let p = rr::decide_publication(&st, t, p.id, p.version, true, None, &approver)
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "authorised");
    let (p, s) = rr::publish_publication(&st, t, p.id, p.version, &maker)
        .await
        .unwrap();
    assert_eq!((p.status.as_str(), s.published), ("published", 1));

    // ---- correction awaits its decision; approval publishes a revision.
    let r = rr::list_results(&st, t, sid, None, false)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(r.status, ResultStatus::Published);
    let (d, same) = rr::request_decision(
        &st,
        t,
        r.id,
        r.version,
        Some(rr::CorrectionRequest {
            paper_id: setup.paper_id,
            internal_marks: None,
            external_marks: Some(dec("66")),
            grace_marks: None,
            reason: "re-totalled".into(),
        }),
        None,
        &maker,
    )
    .await
    .unwrap();
    assert_eq!(d.status, "pending");
    assert_eq!((same.id, same.revision), (r.id, 1));
    let (d, revised) = rr::decide_decision(&st, t, d.id, d.version, true, None, &approver)
        .await
        .unwrap();
    assert_eq!(d.status, "approved");
    assert_eq!(
        (revised.revision, revised.status),
        (2, ResultStatus::Published)
    );
    assert_eq!(revised.lines[0].percent, Some(dec("69.60")));

    // ---- session closure awaits its decision: reject reopens, approve closes.
    use sutra_exams::completion::repo as cr;
    let c = cr::initiate_closure(&st, t, sid, &maker).await.unwrap();
    assert_eq!(c.status, "closing");
    let c = cr::close_session(&st, t, sid, c.version, &maker)
        .await
        .unwrap();
    assert_eq!(c.status, "closing");
    assert!(c.process_instance_id.is_some());
    let c = cr::decide_closure(
        &st,
        t,
        sid,
        c.version,
        false,
        Some("not yet".into()),
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(c.status, "reopened");
    let session_status = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, String>("SELECT status FROM exams.exam_session WHERE id = $1")
            .bind(sid.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    assert_eq!(session_status(pool.clone()).await, "reopened");
    let c = cr::initiate_closure(&st, t, sid, &maker).await.unwrap();
    let c = cr::close_session(&st, t, sid, c.version, &maker)
        .await
        .unwrap();
    let c = cr::decide_closure(&st, t, sid, c.version, true, None, &approver)
        .await
        .unwrap();
    assert_eq!(
        (c.status.as_str(), c.completed_registrations),
        ("closed", 1)
    );
    assert_eq!(session_status(pool.clone()).await, "closed");
}

/// L3-I-088 / 089 / 100 (approved correction of an approved result with grace → replacement at
/// the same level), L3-I-099 (governed reopening → fresh working revision), L3-I-087 (recompute
/// of an unchanged result keeps its revision).
#[tokio::test]
async fn reopening_and_pre_publication_replacement() {
    const TENANT_D: &str = "0192f5a0-7c3e-7000-8000-0000000b6c03";
    let (app, pool) = app(TENANT_D).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT_D,
        "Replacement University",
        None,
        "active",
        "conducted",
    )
    .await;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("30", "40")),
        Some("60"),
    )
    .await;
    let st = sutra_api::app::exams_deps_with(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        // Part 1: explicit dev/test auto-approval opt-in (the subject here is not approval).
        sutra_kernel::approval::ApprovalMode::AutoApprove,
    )
    .into_state();
    let t = setup.tenant();
    let sid = setup.session_id;
    let actor = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let b = rr::create_batch(&st, t, sid, &actor).await.unwrap();
    let (b, _) = rr::compute_batch(&st, t, b.id, b.version, &actor)
        .await
        .unwrap();
    let b = rr::validate_batch(&st, t, b.id, b.version, None, &actor)
        .await
        .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Approved);
    let r = rr::list_results(&st, t, sid, None, false)
        .await
        .unwrap()
        .remove(0);
    assert_eq!((r.status, r.revision), (ResultStatus::Approved, 1));
    assert_eq!(r.lines[0].percent, Some(dec("66.00")));

    // Grace marks through an approved correction: 30 + min(60 + 5, 100)/100×60 = 69.00.
    let (d, r2) = rr::request_decision(
        &st,
        t,
        r.id,
        r.version,
        Some(rr::CorrectionRequest {
            paper_id: setup.paper_id,
            internal_marks: None,
            external_marks: None,
            grace_marks: Some(dec("5")),
            reason: "grace under the moderation policy".into(),
        }),
        None,
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(d.status, "approved");
    assert_eq!((r2.status, r2.revision), (ResultStatus::Approved, 2));
    assert_eq!(r2.lines[0].grace_marks, dec("5.00"));
    assert_eq!(r2.lines[0].percent, Some(dec("69.00")));
    let dup =
        rr::request_decision(&st, t, r.id, r.version, None, Some("stale".into()), &actor).await;
    assert!(dup.is_err(), "the superseded revision cannot be acted on");

    // Governed reopening: approved results get a fresh working revision.
    let b = rr::get_batch(&st, t, b.id).await.unwrap();
    let b = rr::reopen_batch(
        &st,
        t,
        b.id,
        b.version,
        "late marks correction window",
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Computed);
    let r3 = rr::list_results(&st, t, sid, None, false)
        .await
        .unwrap()
        .remove(0);
    assert_eq!((r3.status, r3.revision), (ResultStatus::Computed, 3));
    assert_eq!(r3.lines[0].percent, Some(dec("69.00")));
    let same = rr::recompute_result(&st, t, r3.id, r3.version, false, None, &actor)
        .await
        .unwrap();
    assert_eq!(
        (same.id, same.revision),
        (r3.id, 3),
        "unchanged → no new revision"
    );
    let b = rr::validate_batch(&st, t, b.id, b.version, None, &actor)
        .await
        .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Approved);
    let history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM exams.term_result WHERE session_id = $1")
            .bind(sid.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(history, 3, "revisions are kept, never rewritten");
}
