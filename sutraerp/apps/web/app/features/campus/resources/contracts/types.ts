/** ERP_EXTENSIONS 3C wire types (mirror `resources_contract_*` in the OpenAPI). */
export type ContractKind = "vendor" | "service" | "amc" | "lease" | "license" | "nda" | "mou_academic" | "mou_industry" | "mou_research" | "other";
export type ContractStatus = "active" | "draft" | "terminated";
export type Billing = "one-time" | "monthly" | "quarterly" | "annual";
export type Phase = "none" | "evergreen" | "expired" | "notice" | "soon" | "ok";
export type DecisionKind = "notice-given" | "renewing" | "not-renewing" | "snoozed";
export type Owner = "us" | "them" | "mutual";
export type RepeatEvery = "monthly" | "quarterly" | "annually";

export interface Decision {
  kind: DecisionKind;
  date: string | null;
  note: string;
  forTermEnd: string | null;
  until: string | null;
}

export interface RenewalInfo {
  phase: Phase;
  date: string | null;
  daysLeft: number | null;
  noticeDate: string | null;
  noticeDaysLeft: number | null;
  decision: Decision | null;
  handled: boolean;
}

export interface Contract {
  id: string;
  institutionId: string;
  title: string;
  kind: ContractKind;
  status: ContractStatus;
  supplierId: string | null;
  counterpartyName: string;
  counterpartyScope: "national" | "international";
  contactName: string | null;
  contactEmail: string | null;
  contactPhone: string | null;
  value: string | null;
  currency: string;
  billing: Billing;
  annualValue: number;
  startOn: string | null;
  endOn: string | null;
  autoRenew: boolean;
  termMonths: number;
  noticeDays: number;
  purposes: string[];
  tags: string[];
  notes: string | null;
  renewal: RenewalInfo;
  attention: boolean;
  agreementEnd: string | null;
  openObligations: number;
  version: number;
}

export interface Repeat {
  every: RepeatEvery;
  anchor: string;
  until: string | null;
  count: number | null;
  contractEnd: boolean;
}

export interface Obligation {
  id: string;
  contractId: string;
  text: string;
  owner: Owner;
  dueOn: string | null;
  done: boolean;
  repeat: Repeat | null;
  completed: number;
  daysLeft: number | null;
  version: number;
}

export interface Term {
  startOn: string | null;
  endOn: string;
  ended: "renewed" | "notice-given" | "expired" | "terminated";
  value: string | null;
  currency: string;
  note: string;
}

export interface Activity {
  id: string;
  contractId: string;
  heldOn: string;
  kind: string;
  title: string;
  students: number;
  staff: number;
  outcome: string | null;
  evidenceRef: string | null;
}

export interface ContractDetail {
  contract: Contract;
  obligations: Obligation[];
  terms: Term[];
  activities: Activity[];
}

export interface ContractInput {
  title: string;
  kind: ContractKind;
  status?: ContractStatus;
  supplierId?: string | null;
  counterpartyName?: string | null;
  counterpartyScope?: "national" | "international";
  value?: string | null;
  currency?: string;
  billing?: Billing;
  startOn?: string | null;
  endOn?: string | null;
  autoRenew?: boolean;
  termMonths?: number;
  noticeDays?: number;
  purposes?: string[];
  tags?: string[];
  notes?: string | null;
  version?: number;
}

export interface ObligationInput {
  text: string;
  owner?: Owner;
  dueOn?: string | null;
  repeat?: { every: RepeatEvery | "none"; until?: string | null; count?: number | null; contractEnd?: boolean } | null;
  version?: number;
}

export interface ActivityInput {
  heldOn: string;
  kind: string;
  title: string;
  students?: number;
  staff?: number;
  outcome?: string | null;
  evidenceRef?: string | null;
}

export interface ContractStats {
  active: number;
  expiring60: number;
  openObligations: number;
  expired: number;
  annualByCurrency: Record<string, number>;
}

export interface DueObligation {
  obligation: Obligation;
  contractTitle: string;
  counterpartyName: string;
}

export interface ContractDashboard {
  today: string;
  stats: ContractStats;
  attention: Contract[];
  upcoming: Contract[];
  dueObligations: DueObligation[];
}

export interface ContractSettings {
  soonDays: number;
  obligationDays: number;
  defaultNoticeDays: number;
  alarmDays: number[];
}

export interface ReconcileResult {
  contractsUpdated: number;
  termsRecorded: number;
  terminated: number;
  decisionsCleared: number;
}

export interface MouTotals {
  inForce: number;
  functional: number;
  activities: number;
  students: number;
  staff: number;
  functionalByKind: Record<string, number>;
  functionalByScope: Record<string, number>;
  activitiesByKind: Record<string, number>;
}

export interface MouEvidenceRow {
  contractId: string;
  title: string;
  partner: string;
  kind: ContractKind;
  scope: string;
  status: ContractStatus;
  startOn: string | null;
  endsOn: string | null;
  purposes: string[];
  functional: boolean;
  activities: Activity[];
}

export interface MouEvidence {
  year: string;
  from: string;
  to: string;
  totals: MouTotals;
  mous: MouEvidenceRow[];
}
