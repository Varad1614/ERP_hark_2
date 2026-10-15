//! Live Flowable REST (`flowable/flowable-rest:8.0.0`) + PostgreSQL 18 (Batch 9 · 9C, contract §7):
//! deploy every approval BPMN, start three approvals (one auto-approved, two awaiting a
//! decision), complete the two through the outbox decision sync, and assert the history.
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra \
//! SUTRA_TEST_FLOWABLE_URL=http://localhost:8090/flowable-rest SUTRA_TEST_FLOWABLE_PASSWORD=test \
//!   cargo test -p sutra-api --features db-tests,flowable-tests --test live_flowable
//! ```
#![cfg(all(feature = "db-tests", feature = "flowable-tests"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::workflow::{StartProcess, WorkflowPort};
use sutra_kernel::error::KernelError;
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::jobs::{run_once, JobOutcome};
use sutra_platform::workflow_flowable::{
    DecisionSync, FlowableSettings, FlowableWorkflowAdapter, DECISION_SYNC_CONSUMER, PROCESS_KEYS,
};
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b9c00";

fn adapter() -> Arc<FlowableWorkflowAdapter> {
    Arc::new(
        FlowableWorkflowAdapter::new(FlowableSettings {
            url: std::env::var("SUTRA_TEST_FLOWABLE_URL")
                .unwrap_or_else(|_| "http://localhost:8090/flowable-rest".into()),
            user: std::env::var("SUTRA_TEST_FLOWABLE_USER").unwrap_or_else(|_| "rest-admin".into()),
            password: Some(
                std::env::var("SUTRA_TEST_FLOWABLE_PASSWORD").unwrap_or_else(|_| "test".into()),
            ),
            timeout: Duration::from_secs(30),
            deploy_on_start: true,
        })
        .unwrap(),
    )
}

async fn decision_event(
    pool: &sqlx::PgPool,
    scope: &ExecutionScope,
    process_instance_id: &str,
    to_status: &str,
) -> Uuid {
    let env = EventEnvelope::new(
        scope,
        NewEvent {
            source: "/sutraerp/campus",
            event_type: "sutraerp.campus.reservation.status_changed",
            dataschema: "urn:sutraerp:schema:campus.reservation.status_changed:1",
            subject: Some(Uuid::now_v7().to_string()),
            data: serde_json::json!({
                "aggregateId": Uuid::now_v7(),
                "institutionId": scope.institution_id.map(|i| i.as_uuid()),
                "fromStatus": "pending_approval",
                "toStatus": to_status,
                "version": 2,
                "processInstanceId": process_instance_id,
            }),
        },
    );
    sutra_persistence::outbox::enqueue(pool, &env)
        .await
        .unwrap();
    env.id
}

fn start(key: &str, auto: Option<bool>) -> StartProcess {
    let mut variables = BTreeMap::new();
    if let Some(a) = auto {
        variables.insert("autoApprove".to_owned(), serde_json::Value::Bool(a));
    }
    variables.insert(
        "requestedBy".to_owned(),
        serde_json::json!("dev|admin@sutra.dev"),
    );
    StartProcess {
        process_definition_key: key.to_owned(),
        business_key: format!("live-9c:{}:v1", Uuid::now_v7()),
        variables,
    }
}

async fn receipt(pool: &sqlx::PgPool, id: Uuid) -> String {
    sqlx::query_scalar::<_, String>(
        "SELECT outcome FROM platform.event_receipt WHERE consumer = $1 AND event_id = $2",
    )
    .bind(DECISION_SYNC_CONSUMER)
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn approvals_run_on_flowable_and_decisions_sync_from_the_outbox() {
    let pool = support::owner_pool().await;
    let flowable = adapter();
    let mut ready = false;
    for _ in 0..60 {
        if flowable.ping().await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(ready, "Flowable REST did not become ready");

    // Deploy-on-start: every approval key has a deployed definition afterwards.
    flowable.ensure_deployed().await.unwrap();
    let deployed = flowable.deployed_keys().await.unwrap();
    for k in PROCESS_KEYS {
        assert!(deployed.contains(*k), "{k} not deployed");
    }
    assert_eq!(
        flowable.deploy_missing(PROCESS_KEYS).await.unwrap(),
        0,
        "idempotent"
    );

    let tenant = TenantId(Uuid::parse_str(TENANT).unwrap());
    let scope = ExecutionScope::institution(tenant, InstitutionId::new());
    let port: Arc<dyn WorkflowPort> = flowable.clone();

    // 1. Auto-approved (no autoApprove variable → true): ends at `approved`, never `decide`.
    let auto = port
        .start_process(&scope, start("campus.reservation_approval", None))
        .await
        .unwrap();
    assert_eq!(
        flowable
            .instance_ended(&auto.process_instance_id)
            .await
            .unwrap(),
        Some(true)
    );
    let acts = flowable
        .historic_activities(&auto.process_instance_id)
        .await
        .unwrap();
    assert!(acts.iter().any(|a| a == "approved"), "{acts:?}");
    assert!(!acts.iter().any(|a| a == "decide"), "{acts:?}");

    // 2. Two awaiting a decision.
    let approve_req = start("campus.fine_waiver", Some(false));
    let approve = port
        .start_process(&scope, approve_req.clone())
        .await
        .unwrap();
    let reject = port
        .start_process(&scope, start("student.student_suspension", Some(false)))
        .await
        .unwrap();
    for p in [&approve, &reject] {
        assert_eq!(
            flowable
                .instance_ended(&p.process_instance_id)
                .await
                .unwrap(),
            Some(false),
            "{} waits on decide",
            p.process_definition_key
        );
    }
    // A retried command (same key + business key) reuses the live instance.
    let again = port.start_process(&scope, approve_req).await.unwrap();
    assert_eq!(again.process_instance_id, approve.process_instance_id);

    // Another tenant cannot touch the decision.
    let intruder = ExecutionScope::tenant(TenantId::new());
    let foreign = flowable
        .complete_decision(&intruder, &approve.process_instance_id, "approved")
        .await;
    assert!(
        matches!(foreign, Err(KernelError::Forbidden(_))),
        "{foreign:?}"
    );

    // 3. Domain decision events in the outbox complete the `decide` tasks.
    let pending = decision_event(
        &pool,
        &scope,
        &approve.process_instance_id,
        "pending_approval",
    )
    .await;
    let e1 = decision_event(&pool, &scope, &approve.process_instance_id, "approved").await;
    let e2 = decision_event(&pool, &scope, &reject.process_instance_id, "rejected").await;
    let e3 = decision_event(&pool, &scope, &auto.process_instance_id, "approved").await;
    let sync = DecisionSync {
        adapter: flowable.clone(),
        interval: Duration::from_secs(1),
        lookback: Duration::from_secs(3600),
        batch_size: 1000,
    };
    run_once(&pool, &sync).await.unwrap();
    assert_eq!(
        receipt(&pool, pending).await,
        "ignored",
        "submission events never decide"
    );
    assert_eq!(receipt(&pool, e1).await, "processed");
    assert_eq!(receipt(&pool, e2).await, "processed");
    assert_eq!(
        receipt(&pool, e3).await,
        "ignored",
        "auto-approved instance has no decide task"
    );

    for (p, expected) in [(&approve, "approved"), (&reject, "rejected")] {
        assert_eq!(
            flowable
                .instance_ended(&p.process_instance_id)
                .await
                .unwrap(),
            Some(true)
        );
        assert_eq!(
            flowable
                .historic_variable(&p.process_instance_id, "outcome")
                .await
                .unwrap(),
            Some(serde_json::json!(expected))
        );
        assert_eq!(
            flowable
                .historic_variable(&p.process_instance_id, "sutraTenantId")
                .await
                .unwrap(),
            Some(serde_json::json!(TENANT))
        );
        let acts = flowable
            .historic_activities(&p.process_instance_id)
            .await
            .unwrap();
        assert!(acts.iter().any(|a| a == "decide"), "{acts:?}");
        assert!(acts.iter().any(|a| a == "decided"), "{acts:?}");
    }

    // 4. Exactly-once: a second tick finds nothing new.
    assert!(matches!(
        run_once(&pool, &sync).await.unwrap(),
        JobOutcome::Ran { affected: 0 }
    ));
}
