/**
 * ERP_EXTENSIONS 2B wire types (service-desk rules, views, SLA policy, SLA board), straight
 * from the generated OpenAPI client.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

// Condition / action values are free JSON on the wire (strings, numbers, booleans).
type Loose<T> = Omit<T, "value"> & { value?: unknown };
export type Condition = Loose<S["ServiceDeskCondition"]>;
export interface Conditions {
  all?: Condition[];
  any?: Condition[];
}
export type Action = Loose<S["ServiceDeskAction"]>;
export type Rule = Omit<S["ServiceDeskRule"], "conditions" | "actions"> & { conditions: Required<Conditions>; actions: Action[] };
export type RuleInput = Omit<S["ServiceDeskRuleInput"], "conditions" | "actions"> & { conditions?: Conditions; actions: Action[] };
export type RulePreview = S["ServiceDeskRulePreview"];
export type View = Omit<S["ServiceDeskView"], "conditions"> & { conditions: Required<Conditions> };
export type ViewInput = Omit<S["ServiceDeskViewInput"], "conditions"> & { conditions?: Conditions };
export type Queue = S["ServiceDeskQueue"];
export type QueueRow = S["ServiceDeskTicketRow"];
export type SlaState = S["ServiceDeskSlaState"];
export type SlaTarget = S["ServiceDeskSlaTarget"];
export type SlaPolicy = S["ServiceDeskSlaPolicy"];
export type SlaPolicyInput = S["ServiceDeskSlaPolicyInput"];
export type RuleRun = S["ServiceDeskRuleRun"];
