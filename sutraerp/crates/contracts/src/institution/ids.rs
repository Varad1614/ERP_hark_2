//! Typed UUIDv7 ids of the Batch 2 (Domain B) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block; existing ids are never renamed. `InstitutionId` / `TenantId` live in
//! `sutra_kernel::scope` because they are scope roots; they are re-exported here for convenience.

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- 2A: institution lifecycle, policies, organisation, positions ----
typed_id!(
    /// Governed activation request of an institution (one per submission cycle).
    ActivationRequestId
);
typed_id!(
    /// Versioned institutional business policy (not authorization policy).
    InstitutionPolicyId
);
typed_id!(
    /// Node of the organisation-unit tree (faculty, school, department, cell, …).
    OrgUnitId
);
typed_id!(
    /// Organisation type blueprint (allowed unit types + parent/child rules), versioned.
    OrgTypeBlueprintId
);
typed_id!(
    /// Governance configuration of an organisation unit (heads, committees, delegations).
    GovernanceConfigId
);
typed_id!(
    /// Approved organisation-structure transformation (merge / split / move set).
    OrgRestructureId
);
typed_id!(
    /// Effective-dated position definition (not a person or an appointment).
    PositionId
);

// ---- 2B: campuses, academic structure, programmes, curriculum, school model ----
typed_id!(
    /// Institutional campus (physical/operational site owned by Domain B).
    CampusId
);
typed_id!(
    /// Versioned academic structure (levels → streams → disciplines …).
    AcademicStructureId
);
typed_id!(
    /// Programme of the programme catalogue (e.g. B.Tech CSE).
    ProgrammeId
);
typed_id!(
    /// Curriculum framework of a programme (the version series).
    CurriculumId
);
typed_id!(
    /// One immutable curriculum version (Draft → Published → Superseded/Retired).
    CurriculumVersionId
);
typed_id!(
    /// School academic model (grade levels, sections policy, boards) — K-12 institutions.
    SchoolAcademicModelId
);

// ---- 2C: academic calendar, readiness ----
typed_id!(
    /// Academic calendar (the version series for one academic cycle family).
    AcademicCalendarId
);
typed_id!(
    /// One calendar version (Draft → Published → Superseded/Retired).
    CalendarVersionId
);
typed_id!(
    /// Academic year (the open academic context).
    AcademicYearId
);
typed_id!(
    /// Term / semester / trimester inside an academic year.
    AcademicTermId
);
typed_id!(
    /// Key date (holiday, exam window, instruction start, …) of a calendar version.
    KeyDateId
);
typed_id!(
    /// Assignment of a calendar version to a governing scope (institution / campus / unit / programme).
    CalendarAssignmentId
);
typed_id!(
    /// Approved temporary deviation from a calendar (closure, rescheduled day, …).
    CalendarExceptionId
);
typed_id!(
    /// Calendar change simulation (what-if impact assessment).
    CalendarSimulationId
);
