//! Batch 8 foundation test helpers (foundation-owned; slices add their own helpers in their own
//! test file, see `docs/batch8-contracts.md` §7 and §11).
//!
//! * [`campus_setup`] — Batch 7's setup (institution **active** + published programme / calendar
//!   + default chart of accounts) plus an **active Domain B campus** (`campus_id`), so every slice
//!   starts from the same shape.
//! * [`seed_hierarchy`] — site → building → floor → spaces through
//!   `sutra_campus::shared::infrastructure::insert_node` (exactly what 8A's API writes), so 8B
//!   (hostel rooms) and 8C (library / reading rooms) never wait for 8A.
//! * [`seed_node`] — one node under a parent.
//! * [`seed_ticket`] — one service ticket through `shared::tickets::open_ticket` (what 8B / 8C
//!   complaints and 8A requests write).
//! * [`seed_student`] / [`seed_staff`] + [`student_person`] / [`staff_person`] — Domain C / E
//!   persons (residents, passengers, borrowers, requesters) and their campus `PersonRef`.
//! * [`seed_item`], [`seed_store`], [`seed_stock`] — Domain J item master, store and a
//!   `receipt` movement through `sutra_resources::shared::stock::post_movement`, so 8D's inventory
//!   tests do not depend on its own receipt flow.

use super::b3::SeededStudent;
use super::b4;
use super::b7::{self, PeopleSetup, SeededStaff};
use super::Login;
use axum::Router;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::str::FromStr;
use sutra_campus::shared::infrastructure::{insert_node, NewNode};
use sutra_campus::shared::me::PersonRef;
use sutra_campus::shared::rows::{InfrastructureNode, ServiceTicket};
use sutra_campus::shared::tickets::{open_ticket, NewTicket};
use sutra_contracts::campus::status::{
    InfrastructureNodeType, InfrastructureStatus, PersonKind, SpaceType, TicketCategory,
    TicketPriority, TicketSource,
};
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};
use sutra_contracts::resources::ids::{ItemId, StoreLocationId};
use sutra_contracts::resources::status::{ItemKind, StockMovementKind};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_resources::shared::stock::{post_movement, MovementReceipt, NewMovement};
use uuid::Uuid;

/// Decimal from a literal (`"12.50"`).
pub fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("decimal literal")
}

/// `YYYY-MM-DD` date literal.
pub fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("date literal")
}

/// The actor recorded by Campus seeds.
pub fn campus_actor() -> sutra_campus::shared::Actor {
    sutra_campus::shared::Actor {
        id: Some("test:b8-seed".into()),
        correlation_id: None,
    }
}

/// The actor recorded by Resources seeds.
pub fn resources_actor() -> sutra_resources::shared::Actor {
    sutra_resources::shared::Actor {
        id: Some("test:b8-seed".into()),
        correlation_id: None,
    }
}

/// Domain B / C / E / F context a Batch 8 test needs.
#[derive(Clone, Debug)]
pub struct CampusSetup {
    pub people: PeopleSetup,
    /// Active Domain B campus (`B8-MAIN`).
    pub campus_id: Uuid,
}

impl CampusSetup {
    pub fn tenant(&self) -> TenantId {
        self.people.tenant()
    }
    pub fn institution(&self) -> InstitutionId {
        self.people.institution()
    }
    pub fn scope(&self) -> ExecutionScope {
        self.people.scope()
    }
}

/// See the module docs.
pub async fn campus_setup(
    app: &Router,
    admin: &Login,
    owner: &PgPool,
    tenant: &str,
    name: &str,
) -> CampusSetup {
    let people = b7::people_setup(app, admin, owner, tenant, name).await;
    let campus_id: Uuid = sqlx::query_scalar(
        "INSERT INTO institution.campus (tenant_id, institution_id, code, name, campus_type, \
         is_main, timezone, status) VALUES ($1, $2, $3, $4, 'satellite', false, 'Asia/Kolkata', \
         'active') RETURNING id",
    )
    .bind(people.tenant().as_uuid())
    .bind(people.institution().as_uuid())
    .bind(format!("B8{}", &Uuid::now_v7().simple().to_string()[24..]).to_uppercase())
    .bind(format!("{name} — Batch 8 campus"))
    .fetch_one(owner)
    .await
    .expect("seed campus");
    CampusSetup { people, campus_id }
}

/// One active node under `parent` (`None` only for a site). `space_type` only for spaces.
#[allow(clippy::too_many_arguments)]
pub async fn seed_node(
    owner: &PgPool,
    setup: &CampusSetup,
    parent: Option<&InfrastructureNode>,
    node_type: InfrastructureNodeType,
    code: &str,
    name: &str,
    space_type: Option<SpaceType>,
    capacity: Option<i32>,
) -> InfrastructureNode {
    let mut tx = owner.begin().await.expect("tx");
    let n = insert_node(
        &mut tx,
        NewNode {
            tenant: setup.tenant(),
            institution: setup.institution(),
            campus_id: setup.campus_id,
            parent,
            node_type,
            code,
            name,
            space_type,
            capacity,
            floor_number: None,
            area_sq_m: None,
            attributes: serde_json::json!({}),
            status: InfrastructureStatus::Active,
            effective_from: None,
        },
        &campus_actor(),
    )
    .await
    .expect("seed node");
    tx.commit().await.expect("commit");
    n
}

/// A seeded building.
#[derive(Clone, Debug)]
pub struct SeededHierarchy {
    pub site: InfrastructureNode,
    pub building: InfrastructureNode,
    pub floor: InfrastructureNode,
    /// In the order of `spaces` given to [`seed_hierarchy`].
    pub spaces: Vec<InfrastructureNode>,
}

/// Site `<prefix>` → building `<prefix>-B1` → floor `<prefix>-B1-F1` → one space per
/// `(code suffix, type, capacity)` (`<prefix>-B1-<suffix>`), all active. Use a distinct
/// `prefix` per call (codes are unique per institution), e.g. `seed_hierarchy(&pool, &setup,
/// "HST", &[("101", SpaceType::HostelRoom, 2)])`.
pub async fn seed_hierarchy(
    owner: &PgPool,
    setup: &CampusSetup,
    prefix: &str,
    spaces: &[(&str, SpaceType, i32)],
) -> SeededHierarchy {
    use InfrastructureNodeType::*;
    let site = seed_node(
        owner,
        setup,
        None,
        Site,
        prefix,
        &format!("{prefix} site"),
        None,
        None,
    )
    .await;
    let b_code = format!("{prefix}-B1");
    let building = seed_node(
        owner,
        setup,
        Some(&site),
        Building,
        &b_code,
        &format!("{prefix} block 1"),
        None,
        None,
    )
    .await;
    let f_code = format!("{prefix}-B1-F1");
    let floor = seed_node(
        owner,
        setup,
        Some(&building),
        Floor,
        &f_code,
        &format!("{prefix} block 1, floor 1"),
        None,
        None,
    )
    .await;
    let mut out = Vec::new();
    for (suffix, ty, cap) in spaces {
        let code = format!("{prefix}-B1-{suffix}");
        out.push(
            seed_node(
                owner,
                setup,
                Some(&floor),
                Space,
                &code,
                &format!("Room {suffix}"),
                Some(*ty),
                Some(*cap),
            )
            .await,
        );
    }
    SeededHierarchy {
        site,
        building,
        floor,
        spaces: out,
    }
}

/// One open ticket through the shared helper.
pub async fn seed_ticket(
    owner: &PgPool,
    setup: &CampusSetup,
    category: TicketCategory,
    source: TicketSource,
    requester: Option<&PersonRef>,
    node: Option<&InfrastructureNode>,
    title: &str,
) -> ServiceTicket {
    let mut tx = owner.begin().await.expect("tx");
    let t = open_ticket(
        &mut tx,
        &setup.scope(),
        NewTicket {
            institution: setup.institution(),
            category,
            source,
            source_ref_id: None,
            is_complaint: true,
            node_id: node.map(|n| n.id),
            requester,
            title,
            description: Some("seeded"),
            priority: TicketPriority::Medium,
            sla_hours: None,
        },
        &campus_actor(),
    )
    .await
    .expect("seed ticket");
    tx.commit().await.expect("commit");
    t
}

/// An active student with an active context in the setup's institution (`subject_ref` links a
/// login, e.g. `Some(b4::AARAV)`).
pub async fn seed_student(
    owner: &PgPool,
    setup: &CampusSetup,
    given_name: &str,
    subject_ref: Option<&str>,
) -> SeededStudent {
    b4::seed_student(owner, &setup.people.delivery, given_name, subject_ref).await
}

/// An active staff record with an active engagement (`subject_ref` e.g. `Some(b4::MEERA)`).
pub async fn seed_staff(
    owner: &PgPool,
    setup: &CampusSetup,
    given_name: &str,
    subject_ref: Option<&str>,
) -> SeededStaff {
    b7::seed_staff(
        owner,
        &setup.people,
        given_name,
        subject_ref,
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await
}

/// The campus person of a seeded student.
pub fn student_person(s: &SeededStudent, subject_ref: Option<&str>) -> PersonRef {
    PersonRef {
        kind: PersonKind::Student,
        id: s.student_id,
        number: s.student_number.clone(),
        display_name: format!("Seeded {}", s.student_number),
        subject_ref: subject_ref.map(str::to_owned),
    }
}

/// The campus person of a seeded staff member.
pub fn staff_person(s: &SeededStaff) -> PersonRef {
    PersonRef {
        kind: PersonKind::Staff,
        id: s.staff_id.as_uuid(),
        number: s.staff_number.clone(),
        display_name: format!("Seeded {}", s.staff_number),
        subject_ref: s.subject_ref.clone(),
    }
}

/// An active item (`unit = each`, `standard_cost = 100.00`).
pub async fn seed_item(
    owner: &PgPool,
    setup: &CampusSetup,
    code: &str,
    kind: ItemKind,
    reorder_level: Option<&str>,
) -> ItemId {
    ItemId(
        sqlx::query_scalar(
            "INSERT INTO resources.item (tenant_id, institution_id, code, name, kind, unit, \
             reorder_level, standard_cost, status) VALUES ($1, $2, $3, $4, $5, 'each', $6, 100.00, \
             'active') RETURNING id",
        )
        .bind(setup.tenant().as_uuid())
        .bind(setup.institution().as_uuid())
        .bind(code)
        .bind(format!("Item {code}"))
        .bind(kind.as_str())
        .bind(reorder_level.map(dec))
        .fetch_one(owner)
        .await
        .expect("seed item"),
    )
}

/// An active store (`parent = None`) or a location inside one.
pub async fn seed_store(
    owner: &PgPool,
    setup: &CampusSetup,
    code: &str,
    parent: Option<StoreLocationId>,
) -> StoreLocationId {
    StoreLocationId(
        sqlx::query_scalar(
            "INSERT INTO resources.store_location (tenant_id, institution_id, parent_id, code, name) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(setup.tenant().as_uuid())
        .bind(setup.institution().as_uuid())
        .bind(parent.map(|p| p.as_uuid()))
        .bind(code)
        .bind(format!("Store {code}"))
        .fetch_one(owner)
        .await
        .expect("seed store"),
    )
}

/// A `receipt` of `quantity` (positive) into `location` (source `test_seed`, key `posting_key`).
pub async fn seed_stock(
    owner: &PgPool,
    setup: &CampusSetup,
    item: ItemId,
    location: StoreLocationId,
    quantity: &str,
    posting_key: &str,
) -> MovementReceipt {
    let mut tx = owner.begin().await.expect("tx");
    let r = post_movement(
        &mut tx,
        NewMovement {
            tenant: setup.tenant(),
            institution: setup.institution(),
            item_id: item,
            location_id: location,
            kind: StockMovementKind::Receipt,
            quantity: dec(quantity),
            unit_cost: Some(dec("100.00")),
            effective_on: date("2026-11-02"),
            source_type: "test_seed",
            source_id: item.as_uuid(),
            posting_key,
            reverses_entry_id: None,
            reason: Some("seeded"),
            expiry_on: None,
        },
        &resources_actor(),
    )
    .await
    .expect("seed stock");
    tx.commit().await.expect("commit");
    r
}

/// One movement of `kind` through the ledger writer, optionally dated (ERP_EXTENSIONS 2C).
#[allow(clippy::too_many_arguments, dead_code)]
pub async fn seed_movement(
    owner: &PgPool,
    setup: &CampusSetup,
    item: ItemId,
    location: StoreLocationId,
    kind: StockMovementKind,
    quantity: &str,
    effective_on: NaiveDate,
    posting_key: &str,
    expiry_on: Option<NaiveDate>,
) -> MovementReceipt {
    let mut tx = owner.begin().await.expect("tx");
    let r = post_movement(
        &mut tx,
        NewMovement {
            tenant: setup.tenant(),
            institution: setup.institution(),
            item_id: item,
            location_id: location,
            kind,
            quantity: dec(quantity),
            unit_cost: Some(dec("100.00")),
            effective_on,
            source_type: "test_seed",
            source_id: item.as_uuid(),
            posting_key,
            reverses_entry_id: None,
            reason: Some("seeded"),
            expiry_on,
        },
        &resources_actor(),
    )
    .await
    .expect("seed movement");
    tx.commit().await.expect("commit");
    r
}

/// Reverses one ledger entry through the ledger writer (ERP_EXTENSIONS 2C).
#[allow(dead_code)]
pub async fn reverse_entry(
    owner: &PgPool,
    setup: &CampusSetup,
    entry: sutra_contracts::resources::ids::StockLedgerEntryId,
    effective_on: NaiveDate,
) -> MovementReceipt {
    let mut tx = owner.begin().await.expect("tx");
    let r = sutra_resources::shared::stock::reverse_movement(
        &mut tx,
        setup.tenant(),
        entry,
        effective_on,
        Some("test reversal"),
        &resources_actor(),
    )
    .await
    .expect("reverse");
    tx.commit().await.expect("commit");
    r
}
