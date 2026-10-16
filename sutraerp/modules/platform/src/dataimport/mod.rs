//! Data migration — import / export jobs (Part 2 · S4-import, `docs/readiness/part2/s4-import.md`).
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/platform/imports/entities` | `platform.import.manage` |
//! | `GET  /api/v1/platform/imports/entities/{entity}/template?format=` | `platform.import.manage` |
//! | `GET  /api/v1/platform/imports/entities/{entity}/export?institutionId=&format=` | `platform.import.manage` + the entity's manage permission |
//! | `GET  /api/v1/platform/imports/mapping-profiles?entity=` · `POST` (upsert by name) | `platform.import.manage` |
//! | `POST /api/v1/platform/imports` (upload) · `GET` (list) | `platform.import.manage` + entity permission |
//! | `GET  /api/v1/platform/imports/{id}` · `/rows` · `/errors.csv` | `platform.import.manage` |
//! | `PUT  /api/v1/platform/imports/{id}/mapping` | `platform.import.manage` + entity permission |
//! | `POST /api/v1/platform/imports/{id}/dry-run` · `/commit` | `platform.import.manage` + entity permission |
//! | `POST /api/v1/platform/imports/{id}/decision` | `platform.import.approve` (never the job's maker) |
//!
//! Flow: **upload** (CSV / XLSX parsed into `platform.import_job_row`, idempotent on the job key)
//! → **mapping** (source headers → canonical columns; suggested automatically or from a saved
//! profile) → **dry run** (column checks, in-file duplicates by natural key, then every row
//! through the owning module's [`ImportTargetPort`] with `dry_run = true`: the same domain
//! validation as its API, rolled back) → **commit** (batches of `batch_size`, one transaction each,
//! all-or-nothing per batch; rows already present are skipped / updated by natural key, so a
//! re-import or a resumed failed job never duplicates) → **reconciliation** (counts; for opening
//! balances Σ file vs Σ posted vs Σ ledger). Opening fee balances need a second person: the commit
//! request starts `platform.import_commit_approval` and a holder of `platform.import.approve`
//! other than the job's maker decides it. Every step is audited (`platform.import.*`).

pub mod engine;
pub mod files;
pub mod http;

use std::collections::BTreeMap;
use std::sync::Arc;
use sutra_contracts::institution::InstitutionDirectory;
use sutra_contracts::platform::dataimport::ImportTargetPort;
use sutra_contracts::platform::workflow::WorkflowPort;

/// Process definition key of the commit approval (separation of duties).
pub const COMMIT_APPROVAL: &str = "platform.import_commit_approval";

/// Handler state.
#[derive(Clone)]
pub struct ImportState {
    pub pool: sqlx::PgPool,
    /// Registered entities by key (`students`, `staff`, …), wired by the composition root.
    pub targets: Arc<BTreeMap<&'static str, Arc<dyn ImportTargetPort>>>,
    pub workflow: Arc<dyn WorkflowPort>,
    /// Domain B's directory: an upload / export names an institution of the caller's tenant
    /// (404 otherwise, so no job is ever recorded against another tenant's institution).
    pub institutions: Arc<dyn InstitutionDirectory>,
}

impl ImportState {
    pub fn new(
        pool: sqlx::PgPool,
        targets: Vec<Arc<dyn ImportTargetPort>>,
        workflow: Arc<dyn WorkflowPort>,
        institutions: Arc<dyn InstitutionDirectory>,
    ) -> Self {
        let mut map = BTreeMap::new();
        for t in targets {
            map.insert(t.spec().entity, t);
        }
        Self {
            pool,
            targets: Arc::new(map),
            workflow,
            institutions,
        }
    }

    pub fn target(&self, entity: &str) -> Option<&Arc<dyn ImportTargetPort>> {
        self.targets.get(entity)
    }
}

pub use http::router;
