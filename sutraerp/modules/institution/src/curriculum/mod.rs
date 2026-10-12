//! **2B** — academic structure, programme catalogue, curriculum framework (Draft → Published → immutable successor version → Retired), school academic model.
//!
//! Paths: `/api/v1/institution/institutions/{id}/academic-structures|programmes|school-models`,
//! `/api/v1/institution/academic-structures/...`, `/api/v1/institution/programmes/...`,
//! `/api/v1/institution/curricula/...`, `/api/v1/institution/curriculum-versions/...`,
//! `/api/v1/institution/school-models/...`.
//! operation_id prefixes: `academic_structure_`, `programme_`, `curriculum_`, `school_model_`.
//!
//! Owned by slice 2B (see `docs/batch2-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`InstitutionState`](crate::shared::InstitutionState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "curriculum"`.

pub mod domain;
pub mod framework;
pub mod programme;
pub mod publication;
pub mod school;
pub mod structure;
pub mod support;

use crate::shared::InstitutionState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        // academic structure
        .routes(routes!(structure::create, structure::list))
        .routes(routes!(structure::get_one, structure::update))
        .routes(routes!(structure::publish_one))
        .routes(routes!(structure::decision))
        .routes(routes!(structure::revise))
        .routes(routes!(structure::retire))
        // programme catalogue
        .routes(routes!(programme::register, programme::list))
        .routes(routes!(programme::get_one, programme::update))
        .routes(routes!(programme::publish_one))
        .routes(routes!(programme::retire))
        // curriculum framework
        .routes(routes!(framework::create, framework::list))
        .routes(routes!(framework::get_one))
        .routes(routes!(framework::versions))
        .routes(routes!(framework::revise))
        .routes(routes!(framework::retire))
        .routes(routes!(
            framework::get_version_one,
            framework::update_version
        ))
        .routes(routes!(framework::publish_version))
        .routes(routes!(framework::decide_version))
        .routes(routes!(framework::retire_version))
        // school academic model
        .routes(routes!(school::create, school::list))
        .routes(routes!(school::get_one, school::update))
        .routes(routes!(school::publish_one))
        .routes(routes!(school::decision))
        .routes(routes!(school::revise))
        .with_state(state)
}
