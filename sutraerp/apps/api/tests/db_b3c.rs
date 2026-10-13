//! Slice 3C DB tests (owned by 3C; see `docs/batch3-contracts.md` §11). Shared harness:
//! `support::*` and the Batch 3 helpers in `support::b3`.
//!
//! Coverage (L3/L4 C-039 … C-074): qualification completion → conferment → record finalisation
//! → exit (C-039/040), MEME intermediate exit (C-041), withdrawal / transfer-out /
//! discontinuation / termination / administrative / deceased exits + clearance (C-042 … C-048),
//! deferral + remediation (C-049), correction / revocation (C-050), the guardian lifecycle and
//! responsibilities (C-051 … C-061), scholarships (C-062 … C-071), alumni (C-072 … C-074), plus
//! ETag/If-Match, idempotency key, audit + outbox, tenant isolation, permissions and the
//! `ApprovalMode::AwaitDecision` decision endpoints.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use support::b3::{AcademicSetup, SeededStudent};
use support::*;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b3c00";
const OTHER_TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b3c99";
const S: &str = "/api/v1/student";

struct T {
    app: Router,
    pool: PgPool,
    admin: Login,
    setup: AcademicSetup,
}

async fn env(name: &str) -> T {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, TENANT, name).await;
    T {
        app,
        pool,
        admin,
        setup,
    }
}

impl T {
    async fn student(&self, name: &str) -> SeededStudent {
        b3::seed_student(&self.pool, &self.setup, name, None).await
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Value,
        v: Option<i64>,
    ) -> (StatusCode, Value) {
        let req = if method == "GET" {
            get(&self.admin, path)
        } else {
            write(&self.admin, method, path, body, v)
        };
        let (s, _, b) = send(&self.app, req).await;
        (s, b)
    }

    async fn get(&self, path: &str) -> Value {
        let (s, b) = self.call("GET", path, Value::Null, None).await;
        assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
        b
    }

    /// A command that must succeed (200 / 201).
    async fn ok(&self, method: &str, path: &str, body: Value, v: Option<i64>) -> Value {
        let (s, b) = self.call(method, path, body, v).await;
        assert!(
            s == StatusCode::OK || s == StatusCode::CREATED,
            "{method} {path}: {s} {b}"
        );
        b
    }

    /// A command that must fail with `status`.
    async fn fails(
        &self,
        method: &str,
        path: &str,
        body: Value,
        v: Option<i64>,
        status: StatusCode,
    ) -> Value {
        let (s, b) = self.call(method, path, body, v).await;
        assert_eq!(s, status, "{method} {path}: {b}");
        b
    }

    async fn events(&self, subject: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT event_type FROM platform.outbox WHERE subject = $1 ORDER BY created_at, id",
        )
        .bind(subject)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn audits(&self, target: &str) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT action FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at",
        )
        .bind(target)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn context_status(&self, s: &SeededStudent) -> String {
        sqlx::query_scalar("SELECT status FROM student.student_context WHERE id = $1")
            .bind(s.context_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn persona_status(&self, s: &SeededStudent) -> String {
        sqlx::query_scalar("SELECT status FROM student.student WHERE id = $1")
            .bind(s.student_id)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    fn inst(&self) -> String {
        self.setup.institution_id.to_string()
    }
}

fn v(x: &Value) -> i64 {
    x["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {x}"))
}

fn id(x: &Value) -> String {
    x["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {x}"))
        .to_owned()
}

fn item<'a>(c: &'a Value, item: &str) -> &'a Value {
    c["clearances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["item"] == item)
        .unwrap_or_else(|| panic!("no {item} clearance in {c}"))
}

/// Runs a completion of `s` up to `awarded` and returns it.
async fn awarded(t: &T, s: &SeededStudent) -> Value {
    let c = t
        .ok(
            "POST",
            &format!("{S}/students/{}/qualification-completions", s.student_id),
            json!({"outcome": "degree", "clearances": ["finance"]}),
            None,
        )
        .await;
    let c = t
        .ok(
            "POST",
            &format!(
                "{S}/qualification-completions/{}/eligibility-evaluation",
                id(&c)
            ),
            json!({"creditsEarned": 96}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "degree_audit", "{c}");
    let c = t
        .ok(
            "POST",
            &format!("{S}/qualification-completions/{}/degree-audit", id(&c)),
            json!({}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(
        c["status"], "approved",
        "finance-only clearance clears and auto-approves: {c}"
    );
    let c = t
        .ok(
            "POST",
            &format!("{S}/qualification-completions/{}/award", id(&c)),
            json!({}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "awarded");
    c
}

/// Runs a completion of `s` to `exit_completed`.
async fn completed(t: &T, s: &SeededStudent) -> Value {
    let c = awarded(t, s).await;
    let c = t
        .ok(
            "POST",
            &format!("{S}/qualification-completions/{}/finalise", id(&c)),
            json!({}),
            Some(v(&c)),
        )
        .await;
    t.ok(
        "POST",
        &format!("{S}/qualification-completions/{}/complete-exit", id(&c)),
        json!({}),
        Some(v(&c)),
    )
    .await
}

/// L3-C-039 / C-040: eligibility → audit → clearance → approval → conferment → record
/// finalisation → exit completion, with the key rejections.
#[tokio::test]
async fn qualification_completion_conferment_and_exit() {
    let t = env("Completion University").await;
    let s = t.student("Asha").await;
    let base = format!("{S}/students/{}/qualification-completions", s.student_id);
    // Idempotency-Key is required on creates.
    let (st, _, _) = send(
        &t.app,
        Request::post(&base)
            .header("content-type", "application/json")
            .header("cookie", &t.admin.cookie)
            .header("x-csrf-token", &t.admin.csrf)
            .body(Body::from(json!({"outcome": "degree"}).to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let c = t
        .ok("POST", &base, json!({"outcome": "degree"}), None)
        .await;
    assert_eq!(c["status"], "eligibility_evaluation");
    assert_eq!(
        c["requestedClearances"],
        json!(["finance", "library", "department"])
    );
    let cid = id(&c);
    let path = |p: &str| format!("{S}/qualification-completions/{cid}{p}");
    // A second open completion for the same context is refused.
    t.fails(
        "POST",
        &base,
        json!({"outcome": "degree"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // If-Match: 428 when missing, 412 when stale.
    t.fails(
        "POST",
        &path("/eligibility-evaluation"),
        json!({}),
        None,
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    t.fails(
        "POST",
        &path("/eligibility-evaluation"),
        json!({}),
        Some(99),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    // Conferment before approval is illegal.
    t.fails(
        "POST",
        &path("/award"),
        json!({}),
        Some(v(&c)),
        StatusCode::CONFLICT,
    )
    .await;
    // Not enough credits (curriculum minimum 80): evaluation recorded, state kept.
    let c = t
        .ok(
            "POST",
            &path("/eligibility-evaluation"),
            json!({"creditsEarned": 40}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "eligibility_evaluation");
    assert_eq!(c["eligibilityResult"]["eligible"], false);
    assert_eq!(c["eligibilityResult"]["requiredCredits"], 80.0);
    let c = t
        .ok(
            "POST",
            &path("/eligibility-evaluation"),
            json!({"creditsEarned": 96}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "degree_audit");
    // Audit deficiency keeps the state.
    let c = t
        .ok(
            "POST",
            &path("/degree-audit"),
            json!({"deficiencies": ["project report missing"]}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "degree_audit");
    assert_eq!(c["auditResult"]["passed"], false);
    let c = t
        .ok(
            "POST",
            &path("/degree-audit"),
            json!({"note": "all requirements met"}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "institutional_clearance");
    assert_eq!(
        item(&c, "finance")["status"],
        "cleared",
        "finance from FinanceClearancePort (stub)"
    );
    assert_eq!(item(&c, "library")["status"], "pending");
    // Finance is never resolved manually; waivers need a note.
    let fin = id(item(&c, "finance"));
    t.fails(
        "POST",
        &path(&format!("/clearances/{fin}/resolve")),
        json!({"status": "cleared"}),
        Some(v(&c)),
        StatusCode::CONFLICT,
    )
    .await;
    let lib = id(item(&c, "library"));
    t.fails(
        "POST",
        &path(&format!("/clearances/{lib}/resolve")),
        json!({"status": "waived"}),
        Some(v(&c)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = t
        .ok(
            "POST",
            &path(&format!("/clearances/{lib}/resolve")),
            json!({"status": "cleared"}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "institutional_clearance");
    let dep = id(item(&c, "department"));
    let c = t
        .ok(
            "POST",
            &path(&format!("/clearances/{dep}/resolve")),
            json!({"status": "waived", "note": "no dues"}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(
        c["status"], "approved",
        "auto-approval through the WorkflowPort: {c}"
    );
    assert!(c["processInstanceId"].as_str().is_some());
    let c = t
        .ok(
            "POST",
            &path("/award"),
            json!({"title": "B.Tech in Computer Science"}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "awarded");
    assert!(c["conferredOn"].as_str().is_some());
    let qid = c["conferredQualificationId"].as_str().unwrap().to_owned();
    let q = t.get(&format!("{S}/conferred-qualifications/{qid}")).await;
    assert_eq!(q["title"], "B.Tech in Computer Science");
    assert_eq!(q["status"], "conferred");
    // Exit cannot complete before the record is finalised.
    t.fails(
        "POST",
        &path("/complete-exit"),
        json!({}),
        Some(v(&c)),
        StatusCode::CONFLICT,
    )
    .await;
    let c = t
        .ok("POST", &path("/finalise"), json!({}), Some(v(&c)))
        .await;
    assert_eq!(c["status"], "record_finalized");
    let c = t
        .ok("POST", &path("/complete-exit"), json!({}), Some(v(&c)))
        .await;
    assert_eq!(c["status"], "exit_completed");
    assert_eq!(t.context_status(&s).await, "qualification_completed");
    assert_eq!(t.persona_status(&s).await, "qualification_completed");
    let exits = t.get(&format!("{S}/students/{}/exits", s.student_id)).await;
    assert_eq!(exits["items"][0]["kind"], "completion");
    assert_eq!(exits["items"][0]["status"], "completed");
    // Events + audit in the same transactions.
    let ev = t.events(&cid).await;
    for e in [
        "sutraerp.student.qualification_completion.eligibility_evaluated",
        "sutraerp.student.qualification_completion.degree_audit_completed",
        "sutraerp.student.qualification_completion.institutional_clearance_completed",
        "sutraerp.student.qualification_completion.approved",
        "sutraerp.student.qualification_completion.qualification_awarded",
        "sutraerp.student.qualification_completion.academic_record_finalized",
    ] {
        assert!(ev.iter().any(|x| x == e), "missing {e} in {ev:?}");
    }
    let audits = t.audits(&cid).await;
    assert!(
        audits.contains(&"student.qualification_completion.award".to_owned()),
        "{audits:?}"
    );
    assert!(audits.contains(&"student.qualification_completion.approve".to_owned()));
    let exit_events = t.events(exits["items"][0]["id"].as_str().unwrap()).await;
    assert_eq!(exit_events, vec!["sutraerp.student.student_exit.completed"]);
    // The context is terminal: no new completion or exit (no live context).
    t.fails(
        "POST",
        &base,
        json!({"outcome": "degree"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &format!("{S}/students/{}/exits", s.student_id),
        json!({"kind": "withdrawal", "reason": "x", "clearances": []}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
}

/// L3-C-041: MEME intermediate exit — allowed outcomes, policy requirement, curriculum flag.
#[tokio::test]
async fn meme_intermediate_exit_needs_policy_and_curriculum() {
    let t = env("MEME College").await;
    let s = t.student("Bala").await;
    let base = format!("{S}/students/{}/qualification-completions", s.student_id);
    t.fails(
        "POST",
        &base,
        json!({"outcome": "degree", "pathway": "meme_exit"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = t
        .ok(
            "POST",
            &base,
            json!({"outcome": "certificate", "pathway": "meme_exit"}),
            None,
        )
        .await;
    assert_eq!(c["pathway"], "meme_exit");
    let c = t
        .ok(
            "POST",
            &format!(
                "{S}/qualification-completions/{}/eligibility-evaluation",
                id(&c)
            ),
            json!({"creditsEarned": 44}),
            Some(v(&c)),
        )
        .await;
    // The test curriculum does not permit multiple entry/exit and no policy requirement was given.
    assert_eq!(c["status"], "eligibility_evaluation");
    let unmet = c["eligibilityResult"]["unmet"].as_array().unwrap();
    assert_eq!(unmet.len(), 2, "{unmet:?}");
}

/// L3-C-041 happy path with a curriculum that permits multiple entry/exit (reference port double):
/// the MEME exit reuses the completion lifecycle and closes the context `qualification_completed`.
#[tokio::test]
async fn meme_intermediate_exit_with_mee_curriculum() {
    use std::sync::Arc;
    use sutra_student::completion::domain::*;
    use sutra_student::completion::qualification as q;
    let t = env("MEME University").await;
    let s = t.student("Chitra").await;
    let mut deps = sutra_api::app::student_deps(
        t.pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    );
    deps.references = Arc::new(MeeReferences(
        sutra_institution::PgInstitutionReferences::new(t.pool.clone()),
    ));
    let st = deps.into_state();
    let ctx = admin_ctx();
    let actor = sutra_student::shared::Actor {
        id: Some(ctx.subject.clone()),
        correlation_id: None,
    };
    let sid = sutra_contracts::student::ids::StudentId(s.student_id);
    let c = q::create(
        &st,
        &ctx,
        sid,
        NewQualificationCompletion {
            outcome: sutra_contracts::student::status::QualificationOutcome::Diploma,
            pathway: Some(CompletionPathway::MemeExit),
            clearances: Some(vec![]),
        },
        &actor,
    )
    .await
    .unwrap();
    let ver = |x: i64| sutra_kernel::concurrency::Version(x);
    let c = q::evaluate_eligibility(
        &st,
        &ctx,
        c.id,
        ver(c.version),
        EligibilityEvaluation {
            credits_earned: Some(88.0),
            required_credits: Some(80.0),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(
        c.status.as_str(),
        "degree_audit",
        "{:?}",
        c.eligibility_result
    );
    let c = q::record_audit(
        &st,
        &ctx,
        c.id,
        ver(c.version),
        DegreeAudit::default(),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "approved");
    let c = q::award(
        &st,
        &ctx,
        c.id,
        ver(c.version),
        AwardQualification::default(),
        &actor,
    )
    .await
    .unwrap();
    let c = q::finalise(&st, &ctx, c.id, ver(c.version), &actor)
        .await
        .unwrap();
    let c = q::complete_exit(
        &st,
        &ctx,
        c.id,
        ver(c.version),
        CompleteExit::default(),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "exit_completed");
    assert_eq!(t.context_status(&s).await, "qualification_completed");
    let exits = t.get(&format!("{S}/students/{}/exits", s.student_id)).await;
    assert_eq!(exits["items"][0]["kind"], "meme_exit");
    let quals = t
        .get(&format!(
            "{S}/students/{}/conferred-qualifications",
            s.student_id
        ))
        .await;
    assert_eq!(quals["items"][0]["outcome"], "diploma");
}

/// L3-C-042 / C-046: withdrawal with clearance (finance from Finance; blocked → cleared; waiver).
#[tokio::test]
async fn withdrawal_exit_with_institutional_clearance() {
    let t = env("Withdrawal College").await;
    let s = t.student("Dev").await;
    let base = format!("{S}/students/{}/exits", s.student_id);
    t.fails(
        "POST",
        &base,
        json!({"kind": "completion", "reason": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    t.fails(
        "POST",
        &base,
        json!({"kind": "withdrawal", "reason": " "}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let e = t.ok("POST", &base, json!({"kind": "withdrawal", "reason": "relocating", "clearances": ["finance", "library", "hostel"]}), None).await;
    assert_eq!(e["status"], "clearance_pending");
    let eid = id(&e);
    // A competing exit is refused while this one is open.
    t.fails(
        "POST",
        &base,
        json!({"kind": "discontinuation", "reason": "x"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let path = |p: &str| format!("{S}/exits/{eid}{p}");
    t.fails(
        "POST",
        &path("/complete"),
        json!({}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let lib = id(item(&e, "library"));
    let e = t
        .ok(
            "POST",
            &path(&format!("/clearances/{lib}/resolve")),
            json!({"status": "blocked", "note": "2 books due"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(item(&e, "library")["status"], "blocked");
    let e = t
        .ok(
            "POST",
            &path(&format!("/clearances/{lib}/resolve")),
            json!({"status": "cleared"}),
            Some(v(&e)),
        )
        .await;
    // An item can be added while in clearance.
    let e = t
        .ok(
            "POST",
            &path("/clearances"),
            json!({"item": "department"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["clearances"].as_array().unwrap().len(), 4);
    let hostel = id(item(&e, "hostel"));
    let e = t
        .ok(
            "POST",
            &path(&format!("/clearances/{hostel}/resolve")),
            json!({"status": "waived", "note": "day scholar"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "clearance_pending");
    let dep = id(item(&e, "department"));
    let e = t
        .ok(
            "POST",
            &path(&format!("/clearances/{dep}/resolve")),
            json!({"status": "cleared"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "approved", "{e}");
    let ev = t.events(&eid).await;
    assert!(
        ev.contains(&"sutraerp.student.exit_clearance.completed".to_owned()),
        "{ev:?}"
    );
    let e = t
        .ok(
            "POST",
            &path("/complete"),
            json!({"effectiveOn": "2026-10-01"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "completed");
    assert_eq!(e["effectiveOn"], "2026-10-01");
    assert_eq!(t.context_status(&s).await, "withdrawn");
    assert_eq!(
        t.persona_status(&s).await,
        "active",
        "withdrawal leaves the persona until archival"
    );
    let closed: Option<chrono::NaiveDate> =
        sqlx::query_scalar("SELECT closed_on FROM student.student_context WHERE id = $1")
            .bind(s.context_id)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(closed.unwrap().to_string(), "2026-10-01");
    let ev = t.events(&eid).await;
    assert!(ev.contains(&"sutraerp.student.student_exit.initiated".to_owned()));
    assert!(ev.contains(&"sutraerp.student.student_exit.completed".to_owned()));
    // Completed exits never reopen.
    t.fails(
        "POST",
        &path("/cancel"),
        json!({}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let list = t
        .get(&format!(
            "{S}/institutions/{}/exits?status=completed",
            t.inst()
        ))
        .await;
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"] == eid.as_str()));
}

/// Finance evidence `Outstanding` blocks the finance item; only Finance can clear it.
#[tokio::test]
async fn outstanding_finance_blocks_exit_clearance() {
    use std::sync::Arc;
    use sutra_contracts::finance::*;
    use sutra_student::completion::{domain::*, exit};
    struct Dues;
    #[async_trait::async_trait]
    impl FinanceClearancePort for Dues {
        async fn admission_clearance(
            &self,
            _: &sutra_kernel::scope::ExecutionScope,
            _: AdmissionFinanceQuery,
        ) -> Result<FinanceEvidence, sutra_kernel::error::KernelError> {
            Ok(FinanceEvidence::NotApplicable)
        }
        async fn student_dues(
            &self,
            _: &sutra_kernel::scope::ExecutionScope,
            _: StudentFinanceQuery,
        ) -> Result<FinanceEvidence, sutra_kernel::error::KernelError> {
            Ok(FinanceEvidence::Outstanding {
                detail: "INR 12,000 tuition".into(),
            })
        }
    }
    let t = env("Dues College").await;
    let s = t.student("Esha").await;
    let mut deps = sutra_api::app::student_deps(
        t.pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    );
    deps.finance = Arc::new(Dues);
    let st = deps.into_state();
    let ctx = admin_ctx();
    let actor = sutra_student::shared::Actor {
        id: Some(ctx.subject.clone()),
        correlation_id: None,
    };
    let e = exit::create(
        &st,
        &ctx,
        sutra_contracts::student::ids::StudentId(s.student_id),
        NewStudentExit {
            kind: sutra_contracts::student::status::StudentExitKind::Withdrawal,
            reason: "moving abroad".into(),
            requested_effective_on: None,
            destination_institution: None,
            evidence_ref: None,
            clearances: Some(vec![ClearanceItem::Finance]),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(e.status.as_str(), "clearance_pending");
    assert_eq!(e.clearances[0].status.as_str(), "blocked");
    assert!(e.clearances[0].note.as_deref().unwrap().contains("12,000"));
    // Refreshing still reads Outstanding: unchanged position.
    let e = exit::resolve_clearance(
        &st,
        &ctx,
        e.id,
        e.clearances[0].id,
        sutra_kernel::concurrency::Version(e.version),
        ResolveClearance {
            status: ClearanceResolution::Cleared,
            note: None,
        },
        true,
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(e.status.as_str(), "clearance_pending");
    assert_eq!(e.clearances[0].status.as_str(), "blocked");
}

/// L3-C-043 … C-045, C-047: transfer-out, discontinuation, termination, administrative closure;
/// cancellation re-allows an exit.
#[tokio::test]
async fn transfer_discontinuation_termination_and_administrative_exits() {
    let t = env("Exit Kinds University").await;
    let cases = [
        (
            "transfer",
            json!({"destinationInstitution": "Other University, Pune"}),
            "transferred",
        ),
        ("discontinuation", json!({}), "discontinued"),
        (
            "termination",
            json!({"evidenceRef": "DC/2026/14"}),
            "terminated",
        ),
        ("administrative_closure", json!({}), "discontinued"),
    ];
    for (kind, extra, ctx_status) in cases {
        let s = t.student(kind).await;
        let base = format!("{S}/students/{}/exits", s.student_id);
        let mut body = json!({"kind": kind, "reason": format!("{kind} case"), "clearances": []});
        if kind == "transfer" || kind == "termination" {
            t.fails(
                "POST",
                &base,
                body.clone(),
                None,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await;
        }
        for (k, val) in extra.as_object().unwrap() {
            body[k] = val.clone();
        }
        let e = t.ok("POST", &base, body, None).await;
        assert_eq!(
            e["status"], "approved",
            "{kind}: no clearance items → straight to approval: {e}"
        );
        let e = t
            .ok(
                "POST",
                &format!("{S}/exits/{}/complete", id(&e)),
                json!({}),
                Some(v(&e)),
            )
            .await;
        assert_eq!(e["status"], "completed");
        assert_eq!(t.context_status(&s).await, ctx_status, "{kind}");
    }
    // Cancellation before completion; a new exit is then possible.
    let s = t.student("Farah").await;
    let base = format!("{S}/students/{}/exits", s.student_id);
    let e = t
        .ok(
            "POST",
            &base,
            json!({"kind": "withdrawal", "reason": "x"}),
            None,
        )
        .await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/exits/{}/cancel", id(&e)),
            json!({"reason": "changed mind"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "cancelled");
    assert!(t
        .events(&id(&e))
        .await
        .contains(&"sutraerp.student.student_exit.cancelled".to_owned()));
    assert_eq!(t.context_status(&s).await, "active");
    t.ok(
        "POST",
        &base,
        json!({"kind": "withdrawal", "reason": "again"}),
        None,
    )
    .await;
}

/// L3-C-048: deceased closure needs verified evidence and archives the persona.
#[tokio::test]
async fn deceased_record_closure_archives_the_persona() {
    let t = env("Deceased Closure College").await;
    let s = t.student("Gita").await;
    let base = format!("{S}/students/{}/exits", s.student_id);
    t.fails(
        "POST",
        &base,
        json!({"kind": "deceased", "reason": "notified", "clearances": []}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let e = t.ok("POST", &base, json!({"kind": "deceased", "reason": "notified by family", "evidenceRef": "DC-MH-2026-0042", "clearances": []}), None).await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/exits/{}/complete", id(&e)),
            json!({}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "completed");
    assert_eq!(t.context_status(&s).await, "discontinued");
    assert_eq!(t.persona_status(&s).await, "archived");
}

/// L3-C-049: deferral keeps the governed Deferred position until remediation; cancellation.
#[tokio::test]
async fn completion_deferral_and_remediation() {
    let t = env("Deferral College").await;
    let s = t.student("Hari").await;
    let c = t
        .ok(
            "POST",
            &format!("{S}/students/{}/qualification-completions", s.student_id),
            json!({"outcome": "degree"}),
            None,
        )
        .await;
    let path = |p: &str| format!("{S}/qualification-completions/{}{p}", id(&c));
    t.fails(
        "POST",
        &path("/defer"),
        json!({"reason": ""}),
        Some(v(&c)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = t.ok("POST", &path("/defer"), json!({"reason": "re-evaluation pending", "remediation": "await results", "reviewOn": "2026-12-01"}), Some(v(&c))).await;
    assert_eq!(c["status"], "deferred");
    assert_eq!(c["deferrals"][0]["status"], "approved");
    assert_eq!(c["deferrals"][0]["deferredFrom"], "eligibility_evaluation");
    // No evaluation, approval or conferment while deferred.
    t.fails(
        "POST",
        &path("/eligibility-evaluation"),
        json!({}),
        Some(v(&c)),
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &path("/award"),
        json!({}),
        Some(v(&c)),
        StatusCode::CONFLICT,
    )
    .await;
    let c = t
        .ok(
            "POST",
            &path("/resume"),
            json!({"resolutionNote": "results published"}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "eligibility_evaluation");
    assert_eq!(c["deferrals"][0]["status"], "completed");
    assert!(t
        .events(&id(&c))
        .await
        .contains(&"sutraerp.student.qualification_completion.deferred".to_owned()));
    t.fails(
        "POST",
        &path("/cancel"),
        json!({}),
        Some(v(&c)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = t
        .ok(
            "POST",
            &path("/cancel"),
            json!({"reason": "student withdrew"}),
            Some(v(&c)),
        )
        .await;
    assert_eq!(c["status"], "cancelled");
    // A new completion may start after cancellation.
    t.ok(
        "POST",
        &format!("{S}/students/{}/qualification-completions", s.student_id),
        json!({"outcome": "degree"}),
        None,
    )
    .await;
}

/// L3-C-050: correction appends a version; revocation keeps history; no rewrite.
#[tokio::test]
async fn conferred_qualification_correction_and_revocation() {
    let t = env("Correction University").await;
    let s = t.student("Isha").await;
    let c = awarded(&t, &s).await;
    let qid = c["conferredQualificationId"].as_str().unwrap().to_owned();
    let q = t.get(&format!("{S}/conferred-qualifications/{qid}")).await;
    let base = format!("{S}/conferred-qualifications/{qid}");
    t.fails(
        "POST",
        &format!("{base}/corrections"),
        json!({"proposed": {"title": q["title"]}, "reason": "typo"}),
        Some(v(&q)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    t.fails(
        "POST",
        &format!("{base}/corrections"),
        json!({"proposed": {"title": "B.Tech (Hons)"}, "reason": ""}),
        Some(v(&q)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let old = t.ok("POST", &format!("{base}/corrections"), json!({"proposed": {"title": "B.Tech (Hons) CSE"}, "reason": "honours omitted", "evidenceRef": "BOS-2026-11"}), Some(v(&q))).await;
    assert_eq!(old["status"], "corrected");
    assert_eq!(old["corrections"][0]["resolution"], "corrected");
    let new_id = old["corrections"][0]["resultQualificationId"]
        .as_str()
        .unwrap()
        .to_owned();
    let new = t
        .get(&format!("{S}/conferred-qualifications/{new_id}"))
        .await;
    assert_eq!(new["title"], "B.Tech (Hons) CSE");
    assert_eq!(new["supersedesId"], qid.as_str());
    assert!(t
        .events(&new_id)
        .await
        .contains(&"sutraerp.student.qualification.corrected".to_owned()));
    // The corrected (historical) version cannot change again.
    t.fails(
        "POST",
        &format!("{base}/revoke"),
        json!({"reason": "x"}),
        Some(v(&old)),
        StatusCode::CONFLICT,
    )
    .await;
    let revoked = t
        .ok(
            "POST",
            &format!("{S}/conferred-qualifications/{new_id}/revoke"),
            json!({"reason": "academic misconduct", "evidenceRef": "EC-7"}),
            Some(v(&new)),
        )
        .await;
    assert_eq!(revoked["status"], "revoked");
    assert!(t
        .events(&new_id)
        .await
        .contains(&"sutraerp.student.qualification.revoked".to_owned()));
    let all = t
        .get(&format!(
            "{S}/students/{}/conferred-qualifications",
            s.student_id
        ))
        .await;
    assert_eq!(all["items"].as_array().unwrap().len(), 2, "history kept");
    assert!(
        sqlx::query("DELETE FROM student.conferred_qualification WHERE id = $1::uuid")
            .bind(&qid)
            .execute(&t.pool)
            .await
            .is_err()
    );
}

/// L3-C-051 … C-053, C-059 … C-061: guardian relationship lifecycle and custody.
#[tokio::test]
async fn guardian_relationship_lifecycle_and_custody() {
    let t = env("Guardian School").await;
    let s = t.student("Jaya").await;
    let gbase = format!("{S}/institutions/{}/guardians", t.inst());
    t.fails(
        "POST",
        &gbase,
        json!({"givenName": "Kiran"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let g = t
        .ok(
            "POST",
            &gbase,
            json!({"givenName": "Kiran", "familyName": "Rao", "phone": "+919800000001"}),
            None,
        )
        .await;
    let g = t
        .ok(
            "PATCH",
            &format!("{S}/guardians/{}", id(&g)),
            json!({"occupation": "Engineer"}),
            Some(v(&g)),
        )
        .await;
    assert_eq!(g["occupation"], "Engineer");
    let found = t.get(&format!("{gbase}?q=kiran")).await;
    assert_eq!(found["items"].as_array().unwrap().len(), 1);
    let rbase = format!("{S}/students/{}/guardian-relationships", s.student_id);
    let r = t
        .ok(
            "POST",
            &rbase,
            json!({"guardianId": id(&g), "relationshipType": "father", "isPrimary": true}),
            None,
        )
        .await;
    assert_eq!(r["status"], "proposed");
    let rid = id(&r);
    t.fails(
        "POST",
        &rbase,
        json!({"guardianId": id(&g), "relationshipType": "father"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let p = |x: &str| format!("{S}/guardian-relationships/{rid}{x}");
    t.fails(
        "POST",
        &p("/activate"),
        json!({}),
        Some(v(&r)),
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &p("/submit-verification"),
        json!({"method": "document", "evidenceRef": ""}),
        Some(v(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = t
        .ok(
            "POST",
            &p("/submit-verification"),
            json!({"method": "document", "evidenceRef": "Aadhaar-XXXX1234"}),
            Some(v(&r)),
        )
        .await;
    assert_eq!(r["status"], "verified", "auto-approved verification: {r}");
    let r = t.ok("POST", &p("/activate"), json!({}), Some(v(&r))).await;
    assert_eq!(r["status"], "active");
    let r = t
        .ok(
            "PATCH",
            &p(""),
            json!({"relationshipType": "legal_guardian", "notes": "court appointed"}),
            Some(v(&r)),
        )
        .await;
    assert_eq!(r["relationshipType"], "legal_guardian");
    t.fails(
        "PATCH",
        &p(""),
        json!({}),
        Some(v(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    t.fails(
        "POST",
        &p("/custody"),
        json!({"hasLegalCustody": true, "reason": "court order"}),
        Some(v(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = t.ok("POST", &p("/custody"), json!({"hasLegalCustody": true, "custodyBasis": "Family Court order 77/2026", "reason": "court order"}), Some(v(&r))).await;
    assert_eq!(r["hasLegalCustody"], true);
    let hist = t.get(&p("/custody")).await;
    assert_eq!(hist["items"].as_array().unwrap().len(), 1);
    // A pickup authorisation of this guardian is suspended with the relationship and restored.
    let pick = t
        .ok(
            "POST",
            &format!("{S}/students/{}/pickup-authorisations", s.student_id),
            json!({"guardianRelationshipId": rid}),
            None,
        )
        .await;
    t.fails(
        "POST",
        &p("/suspend"),
        json!({}),
        Some(v(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = t
        .ok(
            "POST",
            &p("/suspend"),
            json!({"reason": "safeguarding review"}),
            Some(v(&r)),
        )
        .await;
    assert_eq!(r["status"], "suspended");
    let pick_now = t
        .get(&format!("{S}/pickup-authorisations/{}", id(&pick)))
        .await;
    assert_eq!(pick_now["status"], "suspended");
    // A custodial guardian is reinstated only with a reason.
    t.fails(
        "POST",
        &p("/reinstate"),
        json!({}),
        Some(v(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = t
        .ok(
            "POST",
            &p("/reinstate"),
            json!({"reason": "review closed"}),
            Some(v(&r)),
        )
        .await;
    assert_eq!(r["status"], "active");
    assert_eq!(
        t.get(&format!("{S}/pickup-authorisations/{}", id(&pick)))
            .await["status"],
        "active"
    );
    let r = t
        .ok(
            "POST",
            &p("/terminate"),
            json!({"reason": "guardianship ended"}),
            Some(v(&r)),
        )
        .await;
    assert_eq!(r["status"], "terminated");
    assert_eq!(
        t.get(&format!("{S}/pickup-authorisations/{}", id(&pick)))
            .await["status"],
        "revoked"
    );
    t.fails(
        "POST",
        &p("/reinstate"),
        json!({"reason": "x"}),
        Some(v(&r)),
        StatusCode::CONFLICT,
    )
    .await;
    let ev = t.events(&rid).await;
    for e in [
        "proposed",
        "verified",
        "activated",
        "modified",
        "suspended",
        "reinstated",
        "terminated",
    ] {
        assert!(
            ev.contains(&format!("sutraerp.student.guardian_relationship.{e}")),
            "{e}: {ev:?}"
        );
    }
    assert!(ev.contains(&"sutraerp.student.guardian_custody.changed".to_owned()));
    // A new relationship of the same guardian is possible after termination.
    t.ok(
        "POST",
        &rbase,
        json!({"guardianId": id(&g), "relationshipType": "father"}),
        None,
    )
    .await;
}

async fn active_relationship(t: &T, s: &SeededStudent, name: &str) -> (String, Value) {
    let g = t.ok("POST", &format!("{S}/institutions/{}/guardians", t.inst()), json!({"givenName": name, "phone": "+919800000009", "email": format!("{name}@example.org")}), None).await;
    let r = t
        .ok(
            "POST",
            &format!("{S}/students/{}/guardian-relationships", s.student_id),
            json!({"guardianId": id(&g), "relationshipType": "mother"}),
            None,
        )
        .await;
    let r = t
        .ok(
            "POST",
            &format!("{S}/guardian-relationships/{}/submit-verification", id(&r)),
            json!({"method": "in_person", "evidenceRef": "visit-1"}),
            Some(v(&r)),
        )
        .await;
    let r = t
        .ok(
            "POST",
            &format!("{S}/guardian-relationships/{}/activate", id(&r)),
            json!({}),
            Some(v(&r)),
        )
        .await;
    (id(&r), r)
}

/// L3-C-054 … C-058: consent, communication, emergency contact, financial responsibility, pickup.
#[tokio::test]
async fn guardian_consent_communication_and_responsibilities() {
    let t = env("Contacts School").await;
    let s = t.student("Lata").await;
    let (rid, _) = active_relationship(&t, &s, "Mala").await;
    let p = |x: &str| format!("{S}/guardian-relationships/{rid}{x}");
    // Communication beyond emergencies needs a data-sharing consent.
    t.fails(
        "POST",
        &p("/communications"),
        json!({"channels": ["email"], "topics": ["academic"]}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &p("/communications"),
        json!({"channels": [], "topics": ["academic"]}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let consent = t
        .ok(
            "POST",
            &p("/consents"),
            json!({"consentType": "data_sharing", "evidenceRef": "form-12"}),
            None,
        )
        .await;
    assert_eq!(consent["status"], "granted");
    t.fails(
        "POST",
        &p("/consents"),
        json!({"consentType": "data_sharing"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &p("/consents"),
        json!({"consentType": "field_trip", "validUntil": "2020-01-01"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let comm = t
        .ok(
            "POST",
            &p("/communications"),
            json!({"channels": ["email", "sms"], "topics": ["academic", "attendance"]}),
            None,
        )
        .await;
    assert_eq!(comm["status"], "active");
    t.fails(
        "POST",
        &p("/communications"),
        json!({"channels": ["email"], "topics": ["general"]}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let comm = t
        .ok(
            "POST",
            &p(&format!("/communications/{}/revoke", id(&comm))),
            json!({"reason": "parent request"}),
            Some(v(&comm)),
        )
        .await;
    assert_eq!(comm["status"], "revoked");
    let consent = t
        .ok(
            "POST",
            &p(&format!("/consents/{}/withdraw", id(&consent))),
            json!({"reason": "withdrawn"}),
            Some(v(&consent)),
        )
        .await;
    assert_eq!(consent["status"], "withdrawn");
    let r = t.get(&p("")).await;
    assert_eq!(
        r["consents"].as_array().unwrap().len(),
        1,
        "withdrawal keeps the grant record"
    );
    // Emergency contacts: guardian-based and named; priorities unique.
    let ebase = format!("{S}/students/{}/emergency-contacts", s.student_id);
    let ec = t
        .ok(
            "POST",
            &ebase,
            json!({"guardianRelationshipId": rid, "priority": 1}),
            None,
        )
        .await;
    assert_eq!(ec["phone"], "+919800000009");
    t.fails(
        "POST",
        &ebase,
        json!({"name": "Neighbour", "relation": "neighbour", "phone": "+91990", "priority": 1}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &ebase,
        json!({"name": "Neighbour", "relation": "neighbour", "priority": 2}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let ec2 = t
        .ok(
            "POST",
            &ebase,
            json!({"name": "Neighbour", "relation": "neighbour", "phone": "+91990", "priority": 2}),
            None,
        )
        .await;
    let ec2 = t
        .ok(
            "PATCH",
            &format!("{S}/emergency-contacts/{}", id(&ec2)),
            json!({"priority": 3}),
            Some(v(&ec2)),
        )
        .await;
    assert_eq!(ec2["priority"], 3);
    let ec = t
        .ok(
            "POST",
            &format!("{S}/emergency-contacts/{}/remove", id(&ec)),
            json!({}),
            Some(v(&ec)),
        )
        .await;
    assert_eq!(ec["status"], "revoked");
    let list = t.get(&ebase).await;
    assert_eq!(list["items"][0]["status"], "active");
    // Financial responsibility: shares per type never exceed 100 %.
    let fbase = format!("{S}/students/{}/financial-responsibilities", s.student_id);
    let f = t.ok("POST", &fbase, json!({"guardianRelationshipId": rid, "responsibilityType": "tuition", "sharePercent": 60}), None).await;
    t.fails(
        "POST",
        &fbase,
        json!({"guardianRelationshipId": rid, "responsibilityType": "all", "sharePercent": 50}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &fbase,
        json!({"guardianRelationshipId": rid, "responsibilityType": "tuition", "sharePercent": 0}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    t.ok(
        "POST",
        &fbase,
        json!({"guardianRelationshipId": rid, "responsibilityType": "tuition", "sharePercent": 40}),
        None,
    )
    .await;
    let f = t
        .ok(
            "POST",
            &format!("{S}/financial-responsibilities/{}/end", id(&f)),
            json!({"reason": "sponsor took over"}),
            Some(v(&f)),
        )
        .await;
    assert_eq!(f["status"], "revoked");
    assert!(t
        .events(&id(&f))
        .await
        .contains(&"sutraerp.student.financial_responsibility.ended".to_owned()));
    // Pickup: a non-guardian needs an identity document reference.
    let pbase = format!("{S}/students/{}/pickup-authorisations", s.student_id);
    t.fails(
        "POST",
        &pbase,
        json!({"personName": "Driver"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let pk = t.ok("POST", &pbase, json!({"personName": "Driver", "idDocumentRef": "DL-MH12-2020", "validUntil": "2027-03-31"}), None).await;
    let pk = t
        .ok(
            "POST",
            &format!("{S}/pickup-authorisations/{}/revoke", id(&pk)),
            json!({}),
            Some(v(&pk)),
        )
        .await;
    assert_eq!(pk["status"], "revoked");
    // Responsibilities need an active relationship.
    let g2 = t
        .ok(
            "POST",
            &format!("{S}/institutions/{}/guardians", t.inst()),
            json!({"givenName": "Nina", "phone": "+91981"}),
            None,
        )
        .await;
    let r2 = t
        .ok(
            "POST",
            &format!("{S}/students/{}/guardian-relationships", s.student_id),
            json!({"guardianId": id(&g2), "relationshipType": "relative"}),
            None,
        )
        .await;
    t.fails(
        "POST",
        &format!("{S}/guardian-relationships/{}/consents", id(&r2)),
        json!({"consentType": "photography"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "POST",
        &pbase,
        json!({"guardianRelationshipId": id(&r2)}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
}

async fn active_scholarship(
    t: &T,
    code: &str,
    criteria: Value,
    renewable: bool,
    scheme: Option<&str>,
) -> Value {
    let sch = t.ok("POST", &format!("{S}/institutions/{}/scholarships", t.inst()), json!({
        "code": code, "name": format!("{code} scholarship"), "sponsorType": "institutional",
        "benefitType": "fee_percentage", "benefitValue": 50, "validFrom": "2026-01-01", "validUntil": "2027-12-31",
        "renewable": renewable, "externalScheme": scheme,
    }), None).await;
    let sch = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{}/publish", id(&sch)),
            json!({}),
            Some(v(&sch)),
        )
        .await;
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{}/policies", id(&sch)),
            json!({"criteria": criteria}),
            None,
        )
        .await;
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarship-policies/{}/submit", id(&p)),
            json!({}),
            Some(v(&p)),
        )
        .await;
    assert_eq!(p["status"], "effective", "{p}");
    let sch = t.get(&format!("{S}/scholarships/{}", id(&sch))).await;
    t.ok(
        "POST",
        &format!("{S}/scholarships/{}/activate", id(&sch)),
        json!({}),
        Some(v(&sch)),
    )
    .await
}

fn merit() -> Value {
    json!({"minAcademicScore": 75, "maxFamilyIncome": 800000, "requiredDocuments": ["income_certificate"], "conditionalEvidence": true, "validityDays": 365})
}

/// L3-C-062 … C-064, C-071: definition + policy lifecycle, determination, recommendation, hand-off.
#[tokio::test]
async fn scholarship_definition_policy_determination_and_hand_off() {
    let t = env("Scholarship University").await;
    let base = format!("{S}/institutions/{}/scholarships", t.inst());
    t.fails("POST", &base, json!({"code": "bad code", "name": "x", "sponsorType": "government", "benefitType": "stipend", "validFrom": "2026-01-01"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let sch = t.ok("POST", &base, json!({"code": "MERIT", "name": "Merit", "sponsorType": "institutional", "benefitType": "fee_waiver", "validFrom": "2026-01-01"}), None).await;
    t.fails("POST", &base, json!({"code": "MERIT", "name": "Dup", "sponsorType": "institutional", "benefitType": "fee_waiver", "validFrom": "2026-01-01"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let sid = id(&sch);
    t.fails(
        "POST",
        &format!("{S}/scholarships/{sid}/activate"),
        json!({}),
        Some(v(&sch)),
        StatusCode::CONFLICT,
    )
    .await;
    let sch = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{sid}/publish"),
            json!({}),
            Some(v(&sch)),
        )
        .await;
    assert_eq!(sch["status"], "published");
    t.fails(
        "POST",
        &format!("{S}/scholarships/{sid}/activate"),
        json!({}),
        Some(v(&sch)),
        StatusCode::CONFLICT,
    )
    .await;
    t.fails(
        "PATCH",
        &format!("{S}/scholarships/{sid}"),
        json!({"name": "Renamed"}),
        Some(v(&sch)),
        StatusCode::CONFLICT,
    )
    .await;
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{sid}/policies"),
            json!({"criteria": merit()}),
            None,
        )
        .await;
    assert_eq!(p["versionNo"], 1);
    t.fails(
        "POST",
        &format!("{S}/scholarships/{sid}/policies"),
        json!({"criteria": merit()}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarship-policies/{}/submit", id(&p)),
            json!({}),
            Some(v(&p)),
        )
        .await;
    assert_eq!(p["status"], "effective");
    assert!(t
        .events(&id(&p))
        .await
        .contains(&"sutraerp.student.scholarship_policy.activated".to_owned()));
    let sch = t.get(&format!("{S}/scholarships/{sid}")).await;
    assert_eq!(sch["effectivePolicy"]["id"], p["id"]);
    let sch = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{sid}/activate"),
            json!({}),
            Some(v(&sch)),
        )
        .await;
    assert_eq!(sch["status"], "active");
    // Determinations.
    let det =
        |s: &SeededStudent| format!("{S}/students/{}/scholarship-eligibilities", s.student_id);
    let low = t.student("Low").await;
    let e = t.ok("POST", &det(&low), json!({"scholarshipId": sid, "facts": {"academicScore": 60, "familyIncome": 300000, "documents": ["income_certificate"]}}), None).await;
    assert_eq!(e["status"], "not_eligible");
    let cond = t.student("Cond").await;
    let e = t
        .ok(
            "POST",
            &det(&cond),
            json!({"scholarshipId": sid, "facts": {"academicScore": 82, "familyIncome": 300000}}),
            None,
        )
        .await;
    assert_eq!(e["status"], "conditionally_eligible");
    assert!(e["conditionDeadline"].as_str().is_some());
    let ok = t.student("Ok").await;
    let e = t.ok("POST", &det(&ok), json!({"scholarshipId": sid, "facts": {"academicScore": 90, "familyIncome": 300000, "documents": ["income_certificate"]}}), None).await;
    assert_eq!(e["status"], "eligible");
    assert_eq!(e["assessments"].as_array().unwrap().len(), 1);
    t.fails(
        "POST",
        &det(&ok),
        json!({"scholarshipId": sid, "facts": {}}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let eid = id(&e);
    t.fails(
        "POST",
        &format!("{S}/scholarship-eligibilities/{eid}/hand-off"),
        json!({}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/scholarship-eligibilities/{eid}/recommend"),
            json!({}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "recommendation_generated");
    let e = t
        .ok(
            "POST",
            &format!("{S}/scholarship-eligibilities/{eid}/hand-off"),
            json!({}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "active");
    assert!(e["validUntil"].as_str().is_some() && e["handedOffAt"].as_str().is_some());
    let ev = t.events(&eid).await;
    for x in ["determined", "recommended", "handed_off"] {
        assert!(
            ev.contains(&format!("sutraerp.student.scholarship_eligibility.{x}")),
            "{x}: {ev:?}"
        );
    }
    let by_sch = t
        .get(&format!("{S}/scholarships/{sid}/eligibilities"))
        .await;
    assert_eq!(by_sch["items"].as_array().unwrap().len(), 3);
    // Suspension needs a reason; a suspended scholarship accepts no determinations.
    t.fails(
        "POST",
        &format!("{S}/scholarships/{sid}/suspend"),
        json!({}),
        Some(v(&sch)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let sch = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{sid}/suspend"),
            json!({"reason": "budget review"}),
            Some(v(&sch)),
        )
        .await;
    let other = t.student("Late").await;
    t.fails(
        "POST",
        &det(&other),
        json!({"scholarshipId": sid, "facts": {}}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let sch = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{sid}/activate"),
            json!({}),
            Some(v(&sch)),
        )
        .await;
    assert_eq!(sch["status"], "active");
}

/// L3-C-065 … C-068: conditions, reassessment, renewal, expiry and archival.
#[tokio::test]
async fn scholarship_conditions_reassessment_renewal_and_expiry() {
    let t = env("Renewal University").await;
    let sch = active_scholarship(&t, "RENEW", merit(), true, None).await;
    let sid = id(&sch);
    let s = t.student("Mohan").await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/students/{}/scholarship-eligibilities", s.student_id),
            json!({"scholarshipId": sid, "facts": {"academicScore": 88, "familyIncome": 200000}}),
            None,
        )
        .await;
    assert_eq!(e["status"], "conditionally_eligible");
    let eid = id(&e);
    let p = |x: &str| format!("{S}/scholarship-eligibilities/{eid}{x}");
    t.fails(
        "POST",
        &p("/resolve-condition"),
        json!({"outcome": "expired"}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let e = t.ok("POST", &p("/resolve-condition"), json!({"outcome": "fulfilled", "facts": {"documents": ["income_certificate"]}, "note": "uploaded"}), Some(v(&e))).await;
    assert_eq!(e["status"], "eligible");
    assert!(t
        .events(&eid)
        .await
        .contains(&"sutraerp.student.scholarship_eligibility.condition_resolved".to_owned()));
    let e = t.ok("POST", &p("/recommend"), json!({}), Some(v(&e))).await;
    let e = t.ok("POST", &p("/hand-off"), json!({}), Some(v(&e))).await;
    assert_eq!(e["status"], "active");
    // Renewal keeps the position for the next period.
    let e = t
        .ok(
            "POST",
            &p("/renew"),
            json!({"facts": {"academicScore": 84}}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "active");
    assert_eq!(e["renewalCount"], 1);
    // Reassessment confirming the outcome keeps active; a changed outcome is recorded without rewrite.
    let e = t
        .ok(
            "POST",
            &p("/reassess"),
            json!({"reason": "annual review"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "active");
    let e = t
        .ok(
            "POST",
            &p("/reassess"),
            json!({"facts": {"academicScore": 50}, "reason": "grades dropped"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "not_eligible");
    let full = t.get(&p("")).await;
    let kinds: Vec<&str> = full["assessments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "determination",
            "condition_resolution",
            "renewal",
            "reassessment",
            "reassessment"
        ]
    );
    assert!(sqlx::query("UPDATE student.scholarship_eligibility_assessment SET reason = 'x' WHERE eligibility_id = $1::uuid").bind(&eid).execute(&t.pool).await.is_err());
    t.fails(
        "POST",
        &p("/renew"),
        json!({}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let e = t
        .ok(
            "POST",
            &p("/archive"),
            json!({"reason": "closed"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "archived");
    // Expiry of an active eligibility; a non-renewable scholarship refuses renewal.
    let once = active_scholarship(&t, "ONCE", json!({"minAcademicScore": 60}), false, None).await;
    let s2 = t.student("Nila").await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/students/{}/scholarship-eligibilities", s2.student_id),
            json!({"scholarshipId": id(&once), "facts": {"academicScore": 70}}),
            None,
        )
        .await;
    let p2 = |x: &str| format!("{S}/scholarship-eligibilities/{}{x}", id(&e));
    let e = t
        .ok("POST", &p2("/recommend"), json!({}), Some(v(&e)))
        .await;
    let e = t.ok("POST", &p2("/hand-off"), json!({}), Some(v(&e))).await;
    t.fails(
        "POST",
        &p2("/renew"),
        json!({}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let e = t
        .ok(
            "POST",
            &p2("/expire"),
            json!({"reason": "academic year ended"}),
            Some(v(&e)),
        )
        .await;
    assert_eq!(e["status"], "expired");
    assert!(t
        .events(&id(&e))
        .await
        .contains(&"sutraerp.student.scholarship_eligibility.expired".to_owned()));
    t.fails(
        "POST",
        &p2("/expire"),
        json!({}),
        Some(v(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let e = t.ok("POST", &p2("/archive"), json!({}), Some(v(&e))).await;
    assert_eq!(e["status"], "archived");
}

/// L3-C-069 / C-070: external sync (no-op port) and bulk recalculation after a policy change.
#[tokio::test]
async fn scholarship_external_sync_and_bulk_recalculation() {
    let t = env("Recalculation University").await;
    let plain = active_scholarship(&t, "PLAIN", json!({"minAcademicScore": 70}), true, None).await;
    t.fails(
        "POST",
        &format!("{S}/scholarships/{}/external-syncs", id(&plain)),
        json!({}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let nsp = active_scholarship(
        &t,
        "NSP-POST",
        json!({"minAcademicScore": 70}),
        true,
        Some("NSP-1001"),
    )
    .await;
    let nid = id(&nsp);
    let mut students = Vec::new();
    for (name, score) in [("Om", 80), ("Pia", 72), ("Ravi", 95)] {
        let s = t.student(name).await;
        let e = t
            .ok(
                "POST",
                &format!("{S}/students/{}/scholarship-eligibilities", s.student_id),
                json!({"scholarshipId": nid, "facts": {"academicScore": score}}),
                None,
            )
            .await;
        let e = t
            .ok(
                "POST",
                &format!("{S}/scholarship-eligibilities/{}/recommend", id(&e)),
                json!({}),
                Some(v(&e)),
            )
            .await;
        let e = t
            .ok(
                "POST",
                &format!("{S}/scholarship-eligibilities/{}/hand-off", id(&e)),
                json!({}),
                Some(v(&e)),
            )
            .await;
        students.push(e);
    }
    let sync = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{nid}/external-syncs"),
            json!({}),
            None,
        )
        .await;
    assert_eq!(sync["outcome"], "not_configured");
    assert_eq!(sync["studentCount"], 3);
    assert!(t
        .events(&id(&sync))
        .await
        .contains(&"sutraerp.student.scholarship_external.synchronised".to_owned()));
    let syncs = t
        .get(&format!("{S}/scholarships/{nid}/external-syncs"))
        .await;
    assert_eq!(syncs["items"].as_array().unwrap().len(), 1);
    // Stricter policy version (minimum 85) supersedes the old one; recalculation applies it.
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarships/{nid}/policies"),
            json!({"criteria": {"minAcademicScore": 85}}),
            None,
        )
        .await;
    assert_eq!(p["versionNo"], 2);
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarship-policies/{}/submit", id(&p)),
            json!({}),
            Some(v(&p)),
        )
        .await;
    assert_eq!(p["status"], "effective");
    let pols = t.get(&format!("{S}/scholarships/{nid}/policies")).await;
    assert!(pols["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["status"] == "superseded"));
    // The policy change alone rewrites nothing.
    assert_eq!(
        t.get(&format!(
            "{S}/scholarship-eligibilities/{}",
            id(&students[0])
        ))
        .await["status"],
        "active"
    );
    let r = t
        .ok(
            "POST",
            &format!("{S}/institutions/{}/scholarship-recalculations", t.inst()),
            json!({"scholarshipId": nid, "reason": "policy v2"}),
            None,
        )
        .await;
    assert_eq!(r["evaluated"], 3);
    assert_eq!(r["changed"], 2);
    assert_eq!(
        t.get(&format!(
            "{S}/scholarship-eligibilities/{}",
            id(&students[0])
        ))
        .await["status"],
        "not_eligible"
    );
    assert_eq!(
        t.get(&format!(
            "{S}/scholarship-eligibilities/{}",
            id(&students[2])
        ))
        .await["status"],
        "active"
    );
    assert!(t
        .events(&id(&r))
        .await
        .contains(&"sutraerp.student.scholarship_recalculation.completed".to_owned()));
    let runs = t
        .get(&format!(
            "{S}/institutions/{}/scholarship-recalculations",
            t.inst()
        ))
        .await;
    assert_eq!(runs["items"].as_array().unwrap().len(), 1);
    t.get(&format!("{S}/scholarship-recalculations/{}", id(&r)))
        .await;
    let p = t
        .ok(
            "POST",
            &format!("{S}/scholarship-policies/{}/retire", id(&p)),
            json!({"reason": "scheme closed"}),
            Some(v(&p)),
        )
        .await;
    assert_eq!(p["status"], "retired");
}

/// L3-C-072 … C-074: alumni transition, correction, archival.
#[tokio::test]
async fn alumni_transition_correction_and_archival() {
    let t = env("Alumni University").await;
    let s = t.student("Sara").await;
    let base = format!("{S}/students/{}/alumni-transitions", s.student_id);
    t.fails("POST", &base, json!({}), None, StatusCode::CONFLICT)
        .await;
    completed(&t, &s).await;
    let a = t.ok("POST", &base, json!({}), None).await;
    assert_eq!(a["status"], "pending_verification");
    t.fails("POST", &base, json!({}), None, StatusCode::CONFLICT)
        .await;
    let p = |x: &str| format!("{S}/alumni-transitions/{}{x}", id(&a));
    t.fails(
        "POST",
        &p("/validate-policy"),
        json!({"graduationYear": 2026}),
        Some(v(&a)),
        StatusCode::CONFLICT,
    )
    .await;
    let a = t
        .ok("POST", &p("/validate-eligibility"), json!({}), Some(v(&a)))
        .await;
    assert_eq!(a["status"], "verified");
    assert_eq!(a["basis"], "qualification");
    let a = t.ok("POST", &p("/validate-policy"), json!({"graduationYear": 2026, "alumniEmail": "sara@alumni.example.org", "directoryConsent": true}), Some(v(&a))).await;
    assert_eq!(a["status"], "approved", "{a}");
    let a = t.ok("POST", &p("/execute"), json!({}), Some(v(&a))).await;
    assert_eq!(a["status"], "activated");
    let a = t.ok("POST", &p("/complete"), json!({}), Some(v(&a))).await;
    assert_eq!(a["status"], "completed");
    assert_eq!(t.persona_status(&s).await, "archived");
    // Corrections on the historical record are governed and versioned.
    t.fails(
        "POST",
        &p("/corrections"),
        json!({"graduationYear": 2026, "reason": "x"}),
        Some(v(&a)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let a = t
        .ok(
            "POST",
            &p("/corrections"),
            json!({"graduationYear": 2027, "reason": "convocation year"}),
            Some(v(&a)),
        )
        .await;
    assert_eq!(a["graduationYear"], 2027);
    assert_eq!(a["correctionCount"], 1);
    assert_eq!(a["corrections"][0]["previous"]["graduationYear"], 2026);
    let a = t
        .ok(
            "POST",
            &p("/archive"),
            json!({"reason": "retention"}),
            Some(v(&a)),
        )
        .await;
    assert_eq!(a["status"], "archived");
    let ev = t.events(&id(&a)).await;
    for x in [
        "initiated",
        "eligibility_validated",
        "policy_validated",
        "approved",
        "student_relationship_closed",
        "alumni_relationship_activated",
        "completed",
        "corrected",
        "archived",
    ] {
        assert!(
            ev.contains(&format!("sutraerp.student.alumni_transition.{x}")),
            "{x}: {ev:?}"
        );
    }
    // An eligible exit (withdrawal) qualifies; a discontinuation does not.
    let w = t.student("Tara").await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/students/{}/exits", w.student_id),
            json!({"kind": "withdrawal", "reason": "job", "clearances": []}),
            None,
        )
        .await;
    t.ok(
        "POST",
        &format!("{S}/exits/{}/complete", id(&e)),
        json!({}),
        Some(v(&e)),
    )
    .await;
    let a = t
        .ok(
            "POST",
            &format!("{S}/students/{}/alumni-transitions", w.student_id),
            json!({}),
            None,
        )
        .await;
    let a = t
        .ok(
            "POST",
            &format!("{S}/alumni-transitions/{}/validate-eligibility", id(&a)),
            json!({}),
            Some(v(&a)),
        )
        .await;
    assert_eq!(a["basis"], "eligible_exit");
    let d = t.student("Uma").await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/students/{}/exits", d.student_id),
            json!({"kind": "discontinuation", "reason": "x", "clearances": []}),
            None,
        )
        .await;
    t.ok(
        "POST",
        &format!("{S}/exits/{}/complete", id(&e)),
        json!({}),
        Some(v(&e)),
    )
    .await;
    t.fails(
        "POST",
        &format!("{S}/students/{}/alumni-transitions", d.student_id),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let a = t
        .ok(
            "POST",
            &format!("{S}/students/{}/alumni-transitions", d.student_id),
            json!({"studentContextId": d.context_id}),
            None,
        )
        .await;
    let a = t
        .ok(
            "POST",
            &format!("{S}/alumni-transitions/{}/validate-eligibility", id(&a)),
            json!({}),
            Some(v(&a)),
        )
        .await;
    assert_eq!(a["status"], "pending_verification");
    assert_eq!(a["eligibilityResult"]["eligible"], false);
}

/// Faculty read only; students hold no 3C permission; another tenant sees nothing.
#[tokio::test]
async fn permissions_and_tenant_isolation() {
    let t = env("Isolation College").await;
    let s = t.student("Vani").await;
    let e = t
        .ok(
            "POST",
            &format!("{S}/students/{}/exits", s.student_id),
            json!({"kind": "withdrawal", "reason": "x"}),
            None,
        )
        .await;
    let faculty = login(&t.app, "meera.shah@sutra.dev").await;
    let (st, _, _) = send(&t.app, get(&faculty, &format!("{S}/exits/{}", id(&e)))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _, _) = send(
        &t.app,
        write(
            &faculty,
            "POST",
            &format!("{S}/exits/{}/cancel", id(&e)),
            json!({}),
            Some(v(&e)),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let student = login(&t.app, "aarav.desai@sutra.dev").await;
    let (st, _, _) = send(&t.app, get(&student, &format!("{S}/exits/{}", id(&e)))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (other, _) = app(OTHER_TENANT).await;
    let admin2 = login(&other, "admin@sutra.dev").await;
    let (st, _, _) = send(&other, get(&admin2, &format!("{S}/exits/{}", id(&e)))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = send(
        &other,
        write(
            &admin2,
            "POST",
            &format!("{S}/students/{}/exits", s.student_id),
            json!({"kind": "withdrawal", "reason": "x"}),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, b) = send(
        &other,
        get(
            &admin2,
            &format!("{S}/students/{}/guardian-relationships", s.student_id),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
}

/// `ApprovalMode::AwaitDecision`: exits, completions, guardian verification, scholarship policy,
/// recommendations, corrections and alumni transitions wait for the explicit decision endpoints.
#[tokio::test]
async fn await_decision_mode_keeps_approvals_pending() {
    use std::sync::Arc;
    use sutra_contracts::student::ids::StudentId;
    use sutra_contracts::student::status::{QualificationOutcome, StudentExitKind};
    use sutra_kernel::concurrency::Version;
    use sutra_student::completion::{domain::*, exit, kit, qualification as q};
    use sutra_student::shared::approval::ApprovalMode;
    let t = env("Awaiting University").await;
    let mut st = sutra_api::app::student_deps(
        t.pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let ctx = admin_ctx();
    let actor = sutra_student::shared::Actor {
        id: Some(ctx.subject.clone()),
        correlation_id: None,
    };
    let decision = |d: kit::LifecycleDecisionValue, r: Option<&str>| kit::LifecycleDecision {
        decision: d,
        reason: r.map(str::to_owned),
    };

    // Exit: pending until decided; rejection needs a reason; the router serves the same state.
    let s = t.student("Waman").await;
    let e = exit::create(
        &st,
        &ctx,
        StudentId(s.student_id),
        NewStudentExit {
            kind: StudentExitKind::Withdrawal,
            reason: "family".into(),
            requested_effective_on: None,
            destination_institution: None,
            evidence_ref: None,
            clearances: Some(vec![]),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(e.status.as_str(), "approval_pending");
    assert!(exit::decide(
        &st,
        &ctx,
        e.id,
        Version(e.version),
        decision(kit::LifecycleDecisionValue::Reject, None),
        &actor
    )
    .await
    .is_err());
    let e = exit::decide(
        &st,
        &ctx,
        e.id,
        Version(e.version),
        decision(kit::LifecycleDecisionValue::Approve, None),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(e.status.as_str(), "approved");
    let _router = sutra_student::router_with_state(st.clone());

    // Completion approval: reject → degree audit; approve → approved.
    let s = t.student("Xavier").await;
    let c = q::create(
        &st,
        &ctx,
        StudentId(s.student_id),
        NewQualificationCompletion {
            outcome: QualificationOutcome::Degree,
            pathway: None,
            clearances: Some(vec![]),
        },
        &actor,
    )
    .await
    .unwrap();
    let c = q::evaluate_eligibility(
        &st,
        &ctx,
        c.id,
        Version(c.version),
        EligibilityEvaluation {
            credits_earned: Some(90.0),
            required_credits: None,
        },
        &actor,
    )
    .await
    .unwrap();
    let c = q::record_audit(
        &st,
        &ctx,
        c.id,
        Version(c.version),
        DegreeAudit::default(),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "approval_pending");
    let c = q::decide(
        &st,
        &ctx,
        c.id,
        Version(c.version),
        decision(
            kit::LifecycleDecisionValue::Reject,
            Some("transcript check"),
        ),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "degree_audit");
    let c = q::record_audit(
        &st,
        &ctx,
        c.id,
        Version(c.version),
        DegreeAudit::default(),
        &actor,
    )
    .await
    .unwrap();
    let c = q::decide(
        &st,
        &ctx,
        c.id,
        Version(c.version),
        decision(kit::LifecycleDecisionValue::Approve, None),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "approved");
    let c = q::award(
        &st,
        &ctx,
        c.id,
        Version(c.version),
        AwardQualification::default(),
        &actor,
    )
    .await
    .unwrap();
    // Correction pending → "no change" outcome keeps the qualification.
    let qid = c.conferred_qualification_id.unwrap();
    let cq = q::get_conferred(&st, ctx.scope.tenant_id, qid)
        .await
        .unwrap();
    let cq = q::request_correction(
        &st,
        &ctx,
        qid,
        Version(cq.version),
        NewQualificationCorrection {
            proposed: ProposedQualification {
                title: Some("Changed".into()),
                ..Default::default()
            },
            reason: "claim".into(),
            evidence_ref: None,
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(cq.corrections[0].status.as_str(), "under_review");
    let cq = q::decide_case(
        &st,
        &ctx,
        qid,
        cq.corrections[0].id,
        Version(cq.version),
        CorrectionDecision {
            decision: CorrectionDecisionValue::NoChange,
            reason: Some("evidence remains valid".into()),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(cq.status.as_str(), "conferred");
    assert_eq!(
        cq.corrections[0].resolution,
        Some(CorrectionResolution::NoChange)
    );

    // Guardian verification and scholarship policy / recommendation decisions.
    use sutra_student::guardian::{domain as gd, repo as gr};
    use sutra_student::scholarship::{domain as sd, repo as sr};
    let inst = sutra_kernel::scope::InstitutionId(t.setup.institution_id);
    let g = gr::create_guardian(
        &st,
        &ctx,
        inst,
        gd::NewGuardian {
            given_name: "Yash".into(),
            family_name: None,
            email: Some("y@example.org".into()),
            phone: None,
            address: None,
            occupation: None,
        },
        &actor,
    )
    .await
    .unwrap();
    let r = gr::propose(
        &st,
        &ctx,
        StudentId(s.student_id),
        gd::NewGuardianRelationship {
            guardian_id: g.id,
            relationship_type: gd::RelationshipType::Father,
            is_primary: false,
            notes: None,
        },
        &actor,
    )
    .await
    .unwrap();
    let r = gr::submit_verification(
        &st,
        &ctx,
        r.id,
        Version(r.version),
        gd::SubmitVerification {
            method: "document".into(),
            evidence_ref: "doc".into(),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(r.status.as_str(), "pending_verification");
    let r = gr::verification_decision(
        &st,
        &ctx,
        r.id,
        Version(r.version),
        decision(kit::LifecycleDecisionValue::Approve, None),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(r.status.as_str(), "verified");

    let sch = sr::create(
        &st,
        &ctx,
        inst,
        sd::NewScholarship {
            code: "AWAIT".into(),
            name: "Await".into(),
            description: None,
            sponsor_type: sd::SponsorType::Government,
            external_scheme: None,
            benefit_type: sd::BenefitType::Stipend,
            benefit_value: Some(1000.0),
            valid_from: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            valid_until: None,
            renewable: false,
        },
        &actor,
    )
    .await
    .unwrap();
    let p = sr::create_policy(
        &st,
        &ctx,
        sch.id,
        sd::NewScholarshipPolicy {
            criteria: sd::PolicyCriteria::default(),
            effective_from: None,
        },
        &actor,
    )
    .await
    .unwrap();
    let p = sr::submit_policy(&st, &ctx, p.id, Version(p.version), &actor)
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "in_review");
    let p = sr::decide_policy(
        &st,
        &ctx,
        p.id,
        Version(p.version),
        decision(kit::LifecycleDecisionValue::Approve, None),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(p.status.as_str(), "effective");
    let sch = sr::get(&st, ctx.scope.tenant_id, sch.id).await.unwrap();
    let sch = sr::move_definition(
        &st,
        &ctx,
        sch.id,
        Version(sch.version),
        sutra_contracts::student::status::ScholarshipDefinitionStatus::Published,
        None,
        &actor,
    )
    .await
    .unwrap();
    sr::move_definition(
        &st,
        &ctx,
        sch.id,
        Version(sch.version),
        sutra_contracts::student::status::ScholarshipDefinitionStatus::Active,
        None,
        &actor,
    )
    .await
    .unwrap();
    let el = sr::determine(
        &st,
        &ctx,
        StudentId(s.student_id),
        sd::NewScholarshipEligibility {
            scholarship_id: sch.id,
            academic_year_id: None,
            facts: sd::EligibilityFacts::default(),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(el.status.as_str(), "eligible");
    let el = sr::recommend(&st, &ctx, el.id, Version(el.version), &actor)
        .await
        .unwrap();
    assert!(el.recommendation_pending && el.status.as_str() == "eligible");
    let el = sr::recommendation_decision(
        &st,
        &ctx,
        el.id,
        Version(el.version),
        decision(kit::LifecycleDecisionValue::Approve, None),
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(el.status.as_str(), "recommendation_generated");
}

// ---- test doubles -----------------------------------------------------------------------------

fn admin_ctx() -> sutra_kernel::auth::AuthContext {
    use sutra_kernel::scope::{ExecutionScope, TenantId};
    let tenant: TenantId = TENANT.parse().unwrap();
    sutra_kernel::auth::AuthContext {
        session_id: uuid::Uuid::now_v7(),
        subject: "dev|admin@sutra.dev".into(),
        login_id: "admin@sutra.dev".into(),
        display_name: "Admin".into(),
        roles: vec![sutra_kernel::auth::Role::Administrator],
        scope: ExecutionScope::tenant(tenant),
        csrf_token: "x".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        idle_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        mfa_level: sutra_kernel::auth::MfaLevel::Mfa,
        amr: vec!["pwd".into()],
        step_up_enforced: false,
    }
}

/// Domain B reference double: every curriculum version permits multiple entry / exit.
struct MeeReferences(sutra_institution::PgInstitutionReferences);

#[async_trait::async_trait]
impl sutra_contracts::institution::reference::InstitutionReferencePort for MeeReferences {
    async fn institution(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: sutra_kernel::scope::InstitutionId,
    ) -> Result<
        Option<sutra_contracts::institution::reference::InstitutionRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.institution(s, id).await
    }
    async fn campus(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Option<sutra_contracts::institution::reference::CampusRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.campus(s, id).await
    }
    async fn org_unit(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Option<sutra_contracts::institution::reference::OrgUnitRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.org_unit(s, id).await
    }
    async fn programme(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Option<sutra_contracts::institution::reference::ProgrammeRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.programme(s, id).await
    }
    async fn curriculum_version(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Option<sutra_contracts::institution::reference::CurriculumVersionRef>,
        sutra_kernel::error::KernelError,
    > {
        Ok(self.0.curriculum_version(s, id).await?.map(|mut c| {
            c.multiple_entry_exit = true;
            c
        }))
    }
    async fn current_curriculum_version(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        p: uuid::Uuid,
        on: chrono::NaiveDate,
    ) -> Result<
        Option<sutra_contracts::institution::reference::CurriculumVersionRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.current_curriculum_version(s, p, on).await
    }
    async fn curriculum_components(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Vec<sutra_contracts::institution::reference::CurriculumComponentRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.curriculum_components(s, id).await
    }
    async fn academic_year(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Option<sutra_contracts::institution::reference::AcademicYearRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.academic_year(s, id).await
    }
    async fn academic_term(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Option<sutra_contracts::institution::reference::AcademicTermRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.academic_term(s, id).await
    }
    async fn key_dates(
        &self,
        s: &sutra_kernel::scope::ExecutionScope,
        id: uuid::Uuid,
    ) -> Result<
        Vec<sutra_contracts::institution::reference::KeyDateRef>,
        sutra_kernel::error::KernelError,
    > {
        self.0.key_dates(s, id).await
    }
}

/// Integration (3B → 3C): completion eligibility reads 3B's `student.qualification_eligibility`,
/// newest determination of the context first, over manual evidence.
#[tokio::test]
async fn completion_reads_progression_qualification_eligibility() {
    let t = env("Eligibility Wiring University").await;
    let s = t.student("Ira").await;
    let insert = |eligible: bool, outcomes: Vec<&'static str>, credits: f64, age_days: i32| {
        sqlx::query(
            "INSERT INTO student.qualification_eligibility (tenant_id, institution_id, student_id, \
             student_context_id, eligible, outcomes, total_credits, basis, evaluated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'ledger', now() - make_interval(days => $8))",
        )
        .bind(t.setup.tenant_id)
        .bind(t.setup.institution_id)
        .bind(s.student_id)
        .bind(s.context_id)
        .bind(eligible)
        .bind(outcomes)
        .bind(credits)
        .bind(age_days)
    };
    // Older determination said eligible; the newest says not yet.
    insert(true, vec!["degree"], 96.0, 3)
        .execute(&t.pool)
        .await
        .unwrap();
    insert(false, vec![], 40.0, 1)
        .execute(&t.pool)
        .await
        .unwrap();
    let c = t
        .ok(
            "POST",
            &format!("{S}/students/{}/qualification-completions", s.student_id),
            json!({"outcome": "degree"}),
            None,
        )
        .await;
    let path = format!(
        "{S}/qualification-completions/{}/eligibility-evaluation",
        id(&c)
    );
    // Manual evidence does not override progression's newest determination.
    let c = t
        .ok("POST", &path, json!({"creditsEarned": 120}), Some(v(&c)))
        .await;
    assert_eq!(c["status"], "eligibility_evaluation");
    assert_eq!(c["eligibilityResult"]["eligible"], false);
    assert_eq!(c["eligibilityResult"]["source"], "progression");
    assert_eq!(c["eligibilityResult"]["creditsEarned"], 40.0);
    // A newer determination makes the student eligible.
    insert(true, vec!["diploma", "degree"], 96.0, 0)
        .execute(&t.pool)
        .await
        .unwrap();
    let c = t.ok("POST", &path, json!({}), Some(v(&c))).await;
    assert_eq!(c["status"], "degree_audit");
    assert_eq!(c["eligibilityResult"]["source"], "progression");
    assert_eq!(c["eligibilityResult"]["creditsEarned"], 96.0);
}

/// Regression (Batch 4 foundation fix, ADR-L5-0036 §3.4): ending a financial responsibility stamps
/// the institution's business-local date, never the UTC date. Between 00:00 and 05:30 IST the UTC
/// date is still "yesterday", so the old `Utc::now().date_naive()` end date preceded a same-day
/// `effective_from`. Asia/Kolkata covers the reported case whenever the run lands in that window;
/// Pacific/Kiritimati (UTC+14) is a day ahead of UTC from 10:00 UTC on, so the same ordering bug
/// is exercised deterministically for most of the day.
#[tokio::test]
async fn guardian_end_date_is_business_local_and_never_precedes_effective_from() {
    let t = env("Business Date School").await;
    for (n, tz) in ["Asia/Kolkata", "Pacific/Kiritimati"]
        .into_iter()
        .enumerate()
    {
        sqlx::query("UPDATE institution.institution SET timezone = $1 WHERE id = $2")
            .bind(tz)
            .bind(t.setup.institution_id)
            .execute(&t.pool)
            .await
            .unwrap();
        let s = t.student(&format!("Tara{n}")).await;
        let (rid, _) = active_relationship(&t, &s, &format!("Usha{n}")).await;
        let fbase = format!("{S}/students/{}/financial-responsibilities", s.student_id);
        let f = t.ok("POST", &fbase, json!({"guardianRelationshipId": rid, "responsibilityType": "tuition", "sharePercent": 100}), None).await;
        let local_today: chrono::NaiveDate =
            sqlx::query_scalar("SELECT (now() AT TIME ZONE $1)::date")
                .bind(tz)
                .fetch_one(&t.pool)
                .await
                .unwrap();
        assert_eq!(f["effectiveFrom"], json!(local_today), "{tz}: {f}");
        let ended = t
            .ok(
                "POST",
                &format!("{S}/financial-responsibilities/{}/end", id(&f)),
                json!({"reason": "sponsor took over"}),
                Some(v(&f)),
            )
            .await;
        assert_eq!(ended["status"], "revoked");
        assert_eq!(ended["effectiveTo"], json!(local_today), "{tz}: {ended}");
        let from: chrono::NaiveDate =
            serde_json::from_value(ended["effectiveFrom"].clone()).unwrap();
        let to: chrono::NaiveDate = serde_json::from_value(ended["effectiveTo"].clone()).unwrap();
        assert!(
            to >= from,
            "{tz}: end date {to} precedes effective_from {from}"
        );
    }
}
