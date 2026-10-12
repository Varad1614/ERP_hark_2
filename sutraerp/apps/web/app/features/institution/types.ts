/**
 * Types of the Batch 2 (Domain B) admin screens — all generated from the OpenAPI document
 * (`@sutra/api-client`, `pnpm gen:api`), so a renamed field fails `tsc`.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

// ---- 2C (generated) -------------------------------------------------------------------------
export type Calendar = S["Calendar"];
export type AcademicYear = S["AcademicYear"];
export type CalendarVersion = S["CalendarVersion"];
export type Term = S["Term"];
export type KeyDate = S["KeyDate"];
export type KeyDateKind = S["KeyDateKind"];
export type CalendarAssignment = S["CalendarAssignment"];
export type CalendarException = S["CalendarException"];
export type ExceptionKind = S["ExceptionKind"];
export type ScopeKind = S["ScopeKind"];
export type CalendarSimulation = S["CalendarSimulation"];
export type ImpactReport = S["ImpactReport"];
export type ImpactItem = S["ImpactItem"];
export type SimulationProposal = S["SimulationProposal"];
export type ClosureEvidence = S["ClosureEvidence"];
export type NewCalendar = S["NewCalendar"];
export type NewAcademicYear = S["NewAcademicYear"];
export type NewCalendarVersion = S["NewCalendarVersion"];
export type NewTerm = S["NewTerm"];
export type NewKeyDate = S["NewKeyDate"];
export type NewAssignment = S["NewAssignment"];
export type NewException = S["NewException"];
export type CloseAcademicYear = S["CloseAcademicYear"];
export type ReadinessReport = S["ReadinessReport"];
export type ReadinessItem = S["ReadinessItem"];
export type ReadinessArea = S["ReadinessArea"];
export type PublicationStatus = S["PublicationStatus"];
export type InstitutionStatus = S["InstitutionStatus"];

// ---- 2A: institution lifecycle, profile, organisation, positions, policies -----------------
export type Institution = S["Institution"];
export type InstitutionProfile = S["InstitutionProfile"];
export type InstitutionType = S["InstitutionType"];
export type AcademicPattern = S["AcademicPattern"];
export type NewInstitution = S["RegisterInstitution"];
export type ReviseProfile = S["ReviseProfile"];
export type OrgTypeBlueprint = S["OrgTypeBlueprint"];
export type UnitType = S["UnitType"];
export type NewBlueprint = S["DraftBlueprint"];
export type OrgUnit = S["OrgUnit"];
export type OrgUnitStatus = S["OrgUnitStatus"];
export type NewOrgUnit = S["RegisterOrgUnit"];
export type Position = S["Position"];
export type PositionStatus = S["PositionStatus"];
export type NewPosition = S["EstablishPosition"];
export type Policy = S["Policy"];
export type PolicyStatus = S["PolicyStatus"];
export type NewPolicy = S["CreatePolicy"];

// ---- 2B: campuses, academic structure, programmes, curriculum, school model ----------------
export type Campus = S["Campus"];
export type CampusStatus = S["CampusStatus"];
export type CampusType = S["CampusType"];
export type NewCampus = S["RegisterCampus"];
export type DecommissionCampus = S["DecommissionCampus"];
export type AcademicStructure = S["AcademicStructure"];
export type AcademicModel = S["AcademicModel"];
export type StructureNode = S["StructureNode"];
export type NewStructure = S["StructureDraft"];
export type Programme = S["Programme"];
export type NewProgramme = S["RegisterProgramme"];
export type Curriculum = S["Curriculum"];
export type NewCurriculum = S["CreateCurriculum"];
export type CurriculumVersion = S["CurriculumVersion"];
export type CurriculumVersionContent = S["CurriculumVersionContent"];
export type CurriculumComponent = S["CurriculumComponent"];
export type ComponentCategory = S["ComponentCategory"];
export type ReviseCurriculum = S["ReviseCurriculum"];
export type SchoolModel = S["SchoolModel"];
export type GradeLevel = S["GradeLevel"];
export type NewSchoolModel = S["SchoolModelDraft"];

/** `{ "items": [...] }` collection (contract §5.1). */
export interface Items<T> {
  items: T[];
}
