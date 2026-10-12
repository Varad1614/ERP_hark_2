/**
 * Types of the slice-3B screens (registration, progression and credits) — generated from the
 * OpenAPI document (`@sutra/api-client`, `pnpm gen:api`), so a renamed field fails `tsc`.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export type RegistrationWindow = S["RegistrationWindow"];
export type CreateWindow = S["CreateWindow"];
export type WindowFinalisation = S["WindowFinalisation"];
export type Registration = S["Registration"];
export type RegistrationLine = S["RegistrationLine"];
export type RegistrationLineKind = S["RegistrationLineKind"];
export type CreateRegistration = S["CreateRegistration"];
export type LineInput = S["LineInput"];
export type RegistrationHold = S["RegistrationHold"];
export type HoldList = S["HoldList"];
export type HoldSource = S["HoldSource"];
export type PlaceHold = S["PlaceHold"];
export type RegistrationOptions = S["RegistrationOptions"];
export type RegistrationOption = S["RegistrationOption"];

export type ProgressionPolicy = S["ProgressionPolicy"];
export type CreateProgressionPolicy = S["CreateProgressionPolicy"];
export type PolicyRules = S["PolicyRules"];
export type ProgressionSummary = S["ProgressionSummary"];
export type ProgressionDecision = S["ProgressionDecision"];
export type ProgressionOutcome = S["ProgressionOutcome"];
export type CreateDecision = S["CreateDecision"];
export type ProgressionCondition = S["ProgressionCondition"];
export type ProbationCase = S["ProbationCase"];
export type CreditLedger = S["CreditLedger"];
export type CreditTransfer = S["CreditTransfer"];
export type ProgrammeMigration = S["ProgrammeMigration"];
export type AcademicBreak = S["AcademicBreak"];
export type CreateBreak = S["CreateBreak"];
export type RejoinBody = S["RejoinBody"];
export type QualificationEligibility = S["QualificationEligibility"];
export type AbcSync = S["AbcSync"];

export interface Items<T> {
  items: T[];
}

export type WindowCommand = "open" | "close" | "cancel";
export type RegistrationCommand = "submit" | "finalise" | "cancel";
export type PolicyCommand = "submit" | "retire";
export type DecisionCommand = "submit" | "discard";
export type ConditionCommand = "fulfil" | "fail" | "waive";
export type ProbationCommand = "clear" | "escalate";
