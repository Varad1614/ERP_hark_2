/**
 * ERP_EXTENSIONS 2B wire types (service-desk rules, views, SLA policy, SLA board), straight
 * from the generated OpenAPI client.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export type Condition = S["ServiceDeskCondition"];
export type Conditions = S["ServiceDeskConditions"];
export type Action = S["ServiceDeskAction"];
export type Rule = S["ServiceDeskRule"];
export type RuleInput = S["ServiceDeskRuleInput"];
export type RulePreview = S["ServiceDeskRulePreview"];
export type View = S["ServiceDeskView"];
export type ViewInput = S["ServiceDeskViewInput"];
export type Queue = S["ServiceDeskQueue"];
export type QueueRow = S["ServiceDeskTicketRow"];
export type SlaState = S["ServiceDeskSlaState"];
export type SlaTarget = S["ServiceDeskSlaTarget"];
export type SlaPolicy = S["ServiceDeskSlaPolicy"];
export type SlaPolicyInput = S["ServiceDeskSlaPolicyInput"];
export type RuleRun = S["ServiceDeskRuleRun"];
