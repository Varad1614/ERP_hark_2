//! Batch 8 foundation smoke test: migrations + the shared Campus / Services / Resources tables,
//! the shared helpers (infrastructure hierarchy rules, legal node / ticket transitions, ticket
//! numbering + SLA + `service_ticket.opened`, the append-only signed stock ledger with on-hand
//! checks, idempotency and reversal), the real `TeachingSpacePort` (Domain D), the cross-domain
//! ports wired for Batch 8 (`CampusChargePort` → a published `campus_charge` in Finance,
//! `ProcurementPostingPort` → balanced AP journal entries, `OperationalMetricsPort` adapters of
//! every domain) and the read-only foundation APIs (infrastructure tree, campus person, metrics
//! catalogue, stock on hand) with their permissions.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use std::sync::Arc;
use support::b8::{self, campus_actor, date, dec, resources_actor};
use support::*;
use sutra_campus::shared::infrastructure::{change_node_status, insert_node, NewNode};
use sutra_campus::shared::lifecycle::StatusChange;
use sutra_campus::shared::rows::{lock_node, lock_ticket};
use sutra_campus::shared::tickets::change_ticket_status;
use sutra_contracts::campus::status::{
    InfrastructureNodeType, InfrastructureStatus, SpaceType, TicketCategory, TicketSource,
    TicketStatus,
};
use sutra_contracts::campus::TeachingSpacePort;
use sutra_contracts::finance::ports::{
    CampusChargeCategory, CampusChargeRequest, ProcurementPostingKind, ProcurementPostingRequest,
};
use sutra_contracts::resources::status::{ItemKind, StockMovementKind};
use sutra_kernel::error::KernelError;
use sutra_resources::shared::stock::{on_hand, post_movement, reverse_movement, NewMovement};
use uuid::Uuid;

const TENANT_CAMPUS: &str = "0192f5a0-7c3e-7000-8000-0000000b8f00";
const TENANT_RES: &str = "0192f5a0-7c3e-7000-8000-0000000b8f01";
const CAMPUS: &str = "/api/v1/campus";

fn change(cause: &'static str) -> StatusChange<'static> {
    StatusChange {
        cause_event_type: cause,
        reason: Some("db_b8_foundation"),
        process_instance_id: None,
    }
}

#[tokio::test]
async fn campus_foundation_tables_helpers_ports_and_directory_api() {
    let (app, pool) = app(TENANT_CAMPUS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_CAMPUS, "Campus University").await;
    let (t, scope) = (setup.tenant(), setup.scope());
    let inst = setup.institution().as_uuid();

    // ---- migrations: every foundation table exists.
    for table in [
        "campus.infrastructure_node",
        "campus.service_ticket",
        "campus.number_sequence",
        "campus.status_history",
        "services.number_sequence",
        "services.status_history",
        "resources.item",
        "resources.store_location",
        "resources.stock_ledger_entry",
        "resources.number_sequence",
        "resources.status_history",
    ] {
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(exists, "{table} exists");
    }

    // ---- infrastructure hierarchy through the shared helper.
    let h = b8::seed_hierarchy(
        &pool,
        &setup,
        "MAIN",
        &[
            ("101", SpaceType::Classroom, 60),
            ("LAB1", SpaceType::Lab, 30),
            ("H01", SpaceType::HostelRoom, 2),
        ],
    )
    .await;
    assert_eq!(h.building.parent_id, Some(h.site.id));
    assert_eq!(h.spaces[0].code, "MAIN-B1-101");
    assert_eq!(h.spaces[0].status, InfrastructureStatus::Active);
    let mut tx = pool.begin().await.unwrap();
    let dup = insert_node(
        &mut tx,
        NewNode {
            tenant: t,
            institution: setup.institution(),
            campus_id: setup.campus_id,
            parent: Some(&h.floor),
            node_type: InfrastructureNodeType::Space,
            code: "MAIN-B1-101",
            name: "Duplicate",
            space_type: Some(SpaceType::Office),
            capacity: None,
            floor_number: None,
            area_sq_m: None,
            attributes: serde_json::json!({}),
            status: InfrastructureStatus::Active,
            effective_from: None,
        },
        &campus_actor(),
    )
    .await;
    assert!(matches!(dup, Err(KernelError::Conflict(_))), "{dup:?}");
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let wrong_parent = insert_node(
        &mut tx,
        NewNode {
            tenant: t,
            institution: setup.institution(),
            campus_id: setup.campus_id,
            parent: Some(&h.site),
            node_type: InfrastructureNodeType::Floor,
            code: "MAIN-F9",
            name: "Floor under a site",
            space_type: None,
            capacity: None,
            floor_number: Some(9),
            area_sq_m: None,
            attributes: serde_json::json!({}),
            status: InfrastructureStatus::Planned,
            effective_from: None,
        },
        &campus_actor(),
    )
    .await;
    assert!(
        matches!(wrong_parent, Err(KernelError::Validation(_))),
        "{wrong_parent:?}"
    );
    tx.rollback().await.unwrap();

    // ---- node lifecycle: active ↔ suspended, illegal moves are conflicts, history appended.
    let lab = h.spaces[1].clone();
    let mut tx = pool.begin().await.unwrap();
    let locked = lock_node(&mut tx, t, lab.id).await.unwrap();
    let suspended = change_node_status(
        &mut tx,
        t,
        &locked,
        InfrastructureStatus::Suspended,
        &change("test.suspend"),
        &campus_actor(),
    )
    .await
    .unwrap();
    assert_eq!(
        (suspended.status, suspended.version),
        (InfrastructureStatus::Suspended, lab.version + 1)
    );
    let illegal = change_node_status(
        &mut tx,
        t,
        &suspended,
        InfrastructureStatus::Planned,
        &change("test.illegal"),
        &campus_actor(),
    )
    .await;
    assert!(matches!(illegal, Err(KernelError::Conflict(_))));
    let restored = change_node_status(
        &mut tx,
        t,
        &suspended,
        InfrastructureStatus::Active,
        &change("test.restore"),
        &campus_actor(),
    )
    .await
    .unwrap();
    assert_eq!(restored.status, InfrastructureStatus::Active);
    tx.commit().await.unwrap();
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.status_history WHERE aggregate_type = 'infrastructure_node' \
         AND aggregate_id = $1",
    )
    .bind(lab.id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 3, "created + suspended + restored");
    let delete = sqlx::query("DELETE FROM campus.infrastructure_node WHERE id = $1")
        .bind(lab.id.as_uuid())
        .execute(&pool)
        .await;
    assert!(delete.is_err(), "nodes are never physically deleted");

    // ---- TeachingSpacePort (Domain D) is real now.
    let spaces = sutra_campus::PgTeachingSpaces::new(pool.clone());
    let room = spaces
        .space(&scope, h.spaces[0].id.as_uuid())
        .await
        .unwrap()
        .expect("classroom is a teaching space");
    assert_eq!(
        (
            room.code.as_str(),
            room.kind.as_str(),
            room.capacity,
            room.active
        ),
        ("MAIN-B1-101", "classroom", Some(60), true)
    );
    assert!(spaces
        .space(&scope, h.spaces[2].id.as_uuid())
        .await
        .unwrap()
        .is_none());
    assert!(spaces
        .space(&scope, Uuid::now_v7())
        .await
        .unwrap()
        .is_none());
    let of_campus = spaces
        .spaces_of_campus(&scope, setup.campus_id)
        .await
        .unwrap();
    assert_eq!(
        of_campus
            .iter()
            .map(|s| s.code.as_str())
            .collect::<Vec<_>>(),
        ["MAIN-B1-101", "MAIN-B1-LAB1"]
    );

    // ---- service tickets: numbering, SLA, foundation event, legal transitions only.
    let student = b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let requester = b8::student_person(&student, Some(b4::AARAV));
    let ticket = b8::seed_ticket(
        &pool,
        &setup,
        TicketCategory::Facility,
        TicketSource::SelfService,
        Some(&requester),
        Some(&h.spaces[0]),
        "Projector not working",
    )
    .await;
    assert_eq!(ticket.number, "SR-000001");
    assert_eq!(ticket.status, TicketStatus::Open);
    assert_eq!(ticket.requester_subject.as_deref(), Some(b4::AARAV));
    let sla_hours = (ticket.sla_due_at - ticket.created_at).num_hours();
    assert!(
        (71..=72).contains(&sla_hours),
        "medium priority → 72 h SLA, got {sla_hours}"
    );
    let opened: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(sutra_contracts::campus::events::SERVICE_TICKET_OPENED)
    .bind(ticket.id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(opened, 1);
    let second = b8::seed_ticket(
        &pool,
        &setup,
        TicketCategory::Hostel,
        TicketSource::Hostel,
        None,
        None,
        "Water heater",
    )
    .await;
    assert_eq!(second.number, "SR-000002");
    let mut tx = pool.begin().await.unwrap();
    let mut cur = lock_ticket(&mut tx, t, ticket.id).await.unwrap();
    for to in [
        TicketStatus::Assigned,
        TicketStatus::InProgress,
        TicketStatus::Resolved,
        TicketStatus::Closed,
    ] {
        cur = change_ticket_status(
            &mut tx,
            t,
            &cur,
            to,
            &change("test.ticket"),
            &campus_actor(),
        )
        .await
        .unwrap();
    }
    assert!(cur.resolved_at.is_some() && cur.closed_at.is_some());
    let reopen = change_ticket_status(
        &mut tx,
        t,
        &cur,
        TicketStatus::Reopened,
        &change("test.ticket"),
        &campus_actor(),
    )
    .await;
    assert!(
        matches!(reopen, Err(KernelError::Conflict(_))),
        "closed is final"
    );
    tx.commit().await.unwrap();

    // ---- foundation API: infrastructure tree + campus person.
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!("{CAMPUS}/institutions/{inst}/infrastructure-tree"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 6, "{body}");
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!(
                "{CAMPUS}/institutions/{inst}/infrastructure-tree?nodeType=space&spaceType=lab"
            ),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["code"], "MAIN-B1-LAB1", "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    let (s, _, _) = send(
        &app,
        get(
            &admin,
            &format!("{CAMPUS}/institutions/{inst}/infrastructure-tree?status=bogus"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = send(
        &app,
        get(
            &aarav,
            &format!("{CAMPUS}/institutions/{inst}/infrastructure-tree"),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "students do not read the campus office tree"
    );
    let (s, _, body) = send(&app, get(&aarav, &format!("{CAMPUS}/me/person"))).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["kind"], "student");
    assert_eq!(body["number"], student.student_number.as_str());
    let (s, _, _) = send(&app, get(&admin, &format!("{CAMPUS}/me/person"))).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the admin login is neither student nor staff"
    );

    // ---- CampusChargePort → a published, idempotent `campus_charge` in Finance.
    let state = sutra_api::app::campus_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    let source = Uuid::now_v7();
    let fine = |amount: &str| CampusChargeRequest {
        institution_id: setup.institution(),
        student_id: student.student_id,
        fee_code: "LIB-FINE".into(),
        category: CampusChargeCategory::Library,
        description: "Overdue fine — 3 days".into(),
        amount: dec(amount),
        due_date: Some(date("2026-12-15")),
        source_id: source,
        actor: Some("test:b8".into()),
    };
    let charge = state
        .charges
        .raise_campus_charge(&scope, fine("30.00"))
        .await
        .unwrap();
    assert!(!charge.already_raised && charge.charge_number.starts_with("CHG-"));
    let again = state
        .charges
        .raise_campus_charge(&scope, fine("30.00"))
        .await
        .unwrap();
    assert_eq!(
        (again.charge_id, again.already_raised),
        (charge.charge_id, true)
    );
    let (src, status, outstanding): (String, String, rust_decimal::Decimal) = sqlx::query_as(
        "SELECT source_type, status, outstanding_amount FROM finance.charge WHERE id = $1",
    )
    .bind(charge.charge_id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (src.as_str(), status.as_str(), outstanding),
        ("campus_charge", "published", dec("30.00"))
    );
    let seen = state
        .finance
        .charge(&scope, charge.charge_id)
        .await
        .unwrap()
        .expect("charge readable through ChargeDirectory");
    assert_eq!(seen.outstanding_amount, dec("30.00"));
    let category: String = sqlx::query_scalar(
        "SELECT category FROM finance.fee_head WHERE institution_id = $1 AND code = 'LIB-FINE'",
    )
    .bind(inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(category, "library");
    let bad = state
        .charges
        .raise_campus_charge(
            &scope,
            CampusChargeRequest {
                source_id: Uuid::now_v7(),
                ..fine("-1")
            },
        )
        .await;
    assert!(matches!(bad, Err(KernelError::Validation(_))), "{bad:?}");

    // ---- metrics catalogue (Domain H) over every domain's adapter.
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!("/api/v1/services/institutions/{inst}/metrics"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let value = |key: &str| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["key"] == key)
            .unwrap_or_else(|| panic!("metric {key} in {body}"))["value"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(value("campus.active_spaces"), "3");
    assert_eq!(value("campus.open_tickets"), "1");
    assert!(value("student.active_students").parse::<i64>().unwrap() >= 1);
    assert_eq!(value("finance.receivables_outstanding"), "30");
    for key in [
        "people.working_staff",
        "resources.active_items",
        "finance.payments_collected",
    ] {
        value(key);
    }
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!("/api/v1/services/institutions/{inst}/metrics?domain=campus"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(body["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["domain"] == "campus"));
    let (s, _, _) = send(
        &app,
        get(
            &aarav,
            &format!("/api/v1/services/institutions/{inst}/metrics"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn resources_foundation_stock_ledger_procurement_postings_and_stock_api() {
    let (app, pool) = app(TENANT_RES).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_RES, "Resources University").await;
    let (t, scope) = (setup.tenant(), setup.scope());
    let inst = setup.institution().as_uuid();

    // ---- stock ledger: receipt, idempotent replay, issue, insufficient stock, reversal.
    let item = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, Some("12")).await;
    let store = b8::seed_store(&pool, &setup, "CENTRAL", None).await;
    let r = b8::seed_stock(&pool, &setup, item, store, "10", "line:1").await;
    assert_eq!((r.on_hand_after, r.already_posted), (dec("10"), false));
    let replay = b8::seed_stock(&pool, &setup, item, store, "10", "line:1").await;
    assert_eq!((replay.entry_id, replay.already_posted), (r.entry_id, true));
    let issue_src = Uuid::now_v7();
    let issue = |q: &str, key: &'static str| NewMovement {
        tenant: t,
        institution: setup.institution(),
        item_id: item,
        location_id: store,
        kind: StockMovementKind::Issue,
        quantity: dec(q),
        unit_cost: None,
        effective_on: date("2026-11-03"),
        source_type: "test_issue",
        source_id: issue_src,
        posting_key: key,
        reverses_entry_id: None,
        reason: None,
    };
    let mut tx = pool.begin().await.unwrap();
    let issued = post_movement(&mut tx, issue("-4", "line:1"), &resources_actor())
        .await
        .unwrap();
    assert_eq!(issued.on_hand_after, dec("6"));
    let too_much = post_movement(&mut tx, issue("-7", "line:2"), &resources_actor()).await;
    assert!(
        matches!(too_much, Err(KernelError::Conflict(_))),
        "{too_much:?}"
    );
    let wrong_sign = post_movement(&mut tx, issue("3", "line:3"), &resources_actor()).await;
    assert!(matches!(wrong_sign, Err(KernelError::Validation(_))));
    tx.commit().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let reversed = reverse_movement(
        &mut tx,
        t,
        issued.entry_id,
        date("2026-11-04"),
        Some("issued in error"),
        &resources_actor(),
    )
    .await
    .unwrap();
    assert_eq!(
        (reversed.on_hand_after, reversed.already_posted),
        (dec("10"), false)
    );
    let twice = reverse_movement(
        &mut tx,
        t,
        issued.entry_id,
        date("2026-11-04"),
        None,
        &resources_actor(),
    )
    .await
    .unwrap();
    assert!(twice.already_posted);
    assert_eq!(on_hand(&mut tx, t, item, None).await.unwrap(), dec("10"));
    tx.commit().await.unwrap();
    let rewrite =
        sqlx::query("UPDATE resources.stock_ledger_entry SET quantity = 99 WHERE id = $1")
            .bind(r.entry_id.as_uuid())
            .execute(&pool)
            .await;
    assert!(rewrite.is_err(), "the stock ledger is append-only");

    // ---- stock-on-hand API (resources.read).
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!("/api/v1/resources/institutions/{inst}/stock-on-hand"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body["items"][0]["itemCode"], "PAPER-A4");
    assert_eq!(body["items"][0]["onHand"], "10.000");
    assert_eq!(
        body["items"][0]["atReorderLevel"], true,
        "10 ≤ reorder level 12"
    );
    let (s, _, _) = send(
        &app,
        get(
            &aarav,
            &format!("/api/v1/resources/institutions/{inst}/stock-on-hand"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- ProcurementPostingPort → balanced, idempotent AP entries in Finance.
    let state = sutra_api::app::resources_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    let invoice_id = Uuid::now_v7();
    let invoice = ProcurementPostingRequest {
        institution_id: setup.institution(),
        kind: ProcurementPostingKind::VendorInvoice,
        source_id: invoice_id,
        posting_key: "post".into(),
        posting_date: date("2026-11-30"),
        description: "Invoice VINV-000001".into(),
        supplier_label: "SUP-000001 Acme Traders".into(),
        inventory_amount: dec("1000.00"),
        expense_amount: dec("250.00"),
        payment_amount: dec("0"),
        actor: Some("test:b8".into()),
    };
    let posted = state
        .postings
        .post_procurement(&scope, invoice.clone())
        .await
        .unwrap();
    assert!(!posted.already_posted && posted.entry_number.starts_with("JE-"));
    let replay = state
        .postings
        .post_procurement(&scope, invoice.clone())
        .await
        .unwrap();
    assert_eq!(
        (replay.journal_entry_id, replay.already_posted),
        (posted.journal_entry_id, true)
    );
    let lines = |id: Uuid| {
        let pool = pool.clone();
        async move {
            let r: (rust_decimal::Decimal, rust_decimal::Decimal, i64) = sqlx::query_as(
                "SELECT sum(debit), sum(credit), count(*) FROM finance.journal_line \
                 WHERE journal_entry_id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
            r
        }
    };
    assert_eq!(
        lines(posted.journal_entry_id.as_uuid()).await,
        (dec("1250.00"), dec("1250.00"), 3)
    );
    let payment = state
        .postings
        .post_procurement(
            &scope,
            ProcurementPostingRequest {
                kind: ProcurementPostingKind::VendorPayment,
                source_id: Uuid::now_v7(),
                description: "Payment VPAY-000001".into(),
                inventory_amount: dec("0"),
                expense_amount: dec("0"),
                payment_amount: dec("1250.00"),
                ..invoice.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        lines(payment.journal_entry_id.as_uuid()).await,
        (dec("1250.00"), dec("1250.00"), 2)
    );
    let (source, accounts): (String, i64) = sqlx::query_as(
        "SELECT (SELECT source_type FROM finance.journal_entry WHERE id = $1), \
         (SELECT count(*) FROM finance.ledger_account WHERE institution_id = $2 AND system_key IN \
          ('inventory', 'procurement_expense', 'accounts_payable'))",
    )
    .bind(posted.journal_entry_id.as_uuid())
    .bind(inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((source.as_str(), accounts), ("procurement_invoice", 3));
    let invalid = state
        .postings
        .post_procurement(
            &scope,
            ProcurementPostingRequest {
                source_id: Uuid::now_v7(),
                payment_amount: dec("5.00"),
                ..invoice.clone()
            },
        )
        .await;
    assert!(
        matches!(invalid, Err(KernelError::Validation(_))),
        "{invalid:?}"
    );
}
