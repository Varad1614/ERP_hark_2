/** ERP_EXTENSIONS 2D: wire types of fleet maintenance. */
export interface FleetInterval {
  every: number;
  unit: string;
}

export type DueStatus = "overdue" | "due_soon" | "upcoming" | "snoozed";

export interface ReminderState {
  status: DueStatus;
  nextDate: string | null;
  nextMeter: number | null;
  daysLeft: number | null;
  meterLeft: number | null;
}

export interface Completion {
  date: string;
  meter: number | null;
  entryId: string | null;
  onTime: boolean;
}

export interface ServiceReminder {
  id: string;
  vehicleId: string;
  registrationNo: string;
  task: string;
  schedule: string;
  interval: FleetInterval | null;
  meterInterval: number | null;
  dueSoonDays: number | null;
  dueSoonMeter: number | null;
  lastDate: string | null;
  lastMeter: number | null;
  snoozedUntil: string | null;
  state: ReminderState;
  compliance: number | null;
  history: Completion[];
  active: boolean;
  version: number;
}

export interface ReminderInput {
  task: string;
  interval?: FleetInterval | null;
  meterInterval?: number | null;
  dueSoonDays?: number | null;
  dueSoonMeter?: number | null;
  startDate?: string | null;
  startMeter?: number | null;
  nextDate?: string | null;
  nextMeter?: number | null;
  active?: boolean;
}

export interface Renewal {
  id: string;
  vehicleId: string;
  registrationNo: string;
  renewalType: string;
  label: string | null;
  dueDate: string | null;
  repeat: FleetInterval | null;
  snoozedUntil: string | null;
  status: DueStatus;
  daysLeft: number | null;
  done: boolean;
  history: { date: string; dueDate: string | null; cost: number | null }[];
  version: number;
}

export interface RenewalInput {
  renewalType: string;
  label?: string | null;
  dueDate: string;
  dueSoonDays?: number | null;
  repeat?: FleetInterval | null;
}

export interface BoardRow {
  kind: "service" | "renewal";
  id: string;
  vehicleId: string;
  registrationNo: string;
  title: string;
  status: DueStatus;
  nextDate: string | null;
  nextMeter: number | null;
  daysLeft: number | null;
  meterLeft: number | null;
  version: number;
}

export interface Board {
  asOf: string;
  overdue: number;
  dueSoon: number;
  items: BoardRow[];
}

export interface ServiceEntry {
  id: string;
  servicedOn: string;
  meter: string | null;
  description: string;
  vendor: string | null;
  amount: string;
  reminderIds: string[];
}

export interface VehicleFile {
  vehicleId: string;
  registrationNo: string;
  meter: number | null;
  meterUnit: string;
  fuelCapacity: number | null;
  reminders: ServiceReminder[];
  renewals: Renewal[];
  services: ServiceEntry[];
}

export interface ServiceInput {
  servicedOn?: string | null;
  meter?: string | null;
  description: string;
  vendor?: string | null;
  amount: string;
  reminderIds: string[];
}

export interface FuelInput {
  filledOn?: string | null;
  meter?: string | null;
  volume: string;
  price?: string | null;
  total?: string | null;
  partial?: boolean;
  reset?: boolean;
  kind?: "fuel" | "charge";
}

export interface FuelSummary {
  count: number;
  cost: number;
  volume: number;
  distance: number;
  avgPrice: number | null;
  avgEconomy: number | null;
  costPerMeter: number | null;
}

export interface FuelRow {
  id: string;
  filledOn: string;
  meter: number | null;
  volume: number;
  total: string;
  unitPrice: number | null;
  partial: boolean;
  reset: boolean;
  kind: string;
  derived: { usage: number | null; economy: number | null; costPerMeter: number | null };
}

export interface FuelLog {
  meterUnit: string;
  volumeUnit: string;
  economyUnit: string;
  fuel: FuelSummary;
  charge: FuelSummary;
  items: FuelRow[];
  warnings: string[];
}

export interface CostRow {
  vehicleId: string;
  registrationNo: string;
  fuel: string;
  service: string;
  other: string;
  total: string;
  usage: number | null;
  meterUnit: string;
  costPerMeter: string | null;
}

export interface FleetCosts {
  from: string;
  to: string;
  total: string;
  items: CostRow[];
}

export interface FleetSettings {
  dueSoonDays: number;
  dueSoonMeter: string;
  volumeUnit: string;
}

export interface VehicleLite {
  id: string;
  registrationNo: string;
  label: string;
}
