/** In-memory `FleetApi` for mock mode and screen tests (the server's rules are authoritative). */
import type { FleetApi } from "./api";
import type { BoardRow, FleetSettings, FuelLog, Renewal, ServiceReminder, VehicleFile } from "./types";

const day = (n: number) => new Date(Date.now() + n * 86_400_000).toISOString().slice(0, 10);

function state(nextDate: string | null, nextMeter: number | null, meter: number | null): ServiceReminder["state"] {
  const daysLeft = nextDate ? Math.round((Date.parse(nextDate) - Date.parse(day(0))) / 86_400_000) : null;
  const meterLeft = nextMeter !== null && meter !== null ? nextMeter - meter : null;
  const status = (daysLeft !== null && daysLeft < 0) || (meterLeft !== null && meterLeft <= 0) ? "overdue" : (daysLeft !== null && daysLeft <= 14) || (meterLeft !== null && meterLeft <= 500) ? "due_soon" : "upcoming";
  return { status, nextDate, nextMeter, daysLeft, meterLeft };
}

export function createMockFleetApi(): FleetApi {
  const vehicles = [{ id: "veh-1", registrationNo: "MH15AB0001", label: "Bus 1" }];
  let meter = 20_000;
  let seq = 0;
  const reminders: ServiceReminder[] = [{
    id: "rem-1", vehicleId: "veh-1", registrationNo: "MH15AB0001", task: "Engine oil", schedule: "Every 6 months or 10000 km",
    interval: { every: 6, unit: "months" }, meterInterval: 10_000, dueSoonDays: null, dueSoonMeter: null, lastDate: day(-200), lastMeter: 20_000,
    snoozedUntil: null, state: state(day(-18), 30_000, meter), compliance: null, history: [], active: true, version: 1,
  }];
  const renewals: Renewal[] = [{
    id: "ren-1", vehicleId: "veh-1", registrationNo: "MH15AB0001", renewalType: "insurance", label: null, dueDate: day(10), repeat: { every: 1, unit: "years" },
    snoozedUntil: null, status: "due_soon", daysLeft: 10, done: false, history: [], version: 1,
  }];
  const services: VehicleFile["services"] = [];
  const fuel: FuelLog["items"] = [];
  let settings: FleetSettings = { dueSoonDays: 14, dueSoonMeter: "500", volumeUnit: "L" };
  const file = async (): Promise<VehicleFile> => ({ vehicleId: "veh-1", registrationNo: "MH15AB0001", meter, meterUnit: "km", fuelCapacity: 50, reminders, renewals, services });
  const summary = { count: 0, cost: 0, volume: 0, distance: 0, avgPrice: null, avgEconomy: null, costPerMeter: null };
  const log = async (warnings: string[] = []): Promise<FuelLog> => ({ meterUnit: "km", volumeUnit: "L", economyUnit: "L/100 km", fuel: { ...summary, count: fuel.length }, charge: summary, items: fuel, warnings });
  return {
    vehicles: async () => vehicles,
    board: async () => {
      const items: BoardRow[] = [
        ...reminders.filter((r) => r.state.status !== "upcoming").map((r): BoardRow => ({ kind: "service", id: r.id, vehicleId: r.vehicleId, registrationNo: r.registrationNo, title: r.task, status: r.state.status, nextDate: r.state.nextDate, nextMeter: r.state.nextMeter, daysLeft: r.state.daysLeft, meterLeft: r.state.meterLeft, version: r.version })),
        ...renewals.filter((r) => !r.done && r.status !== "upcoming").map((r): BoardRow => ({ kind: "renewal", id: r.id, vehicleId: r.vehicleId, registrationNo: r.registrationNo, title: r.label ?? r.renewalType, status: r.status, nextDate: r.dueDate, nextMeter: null, daysLeft: r.daysLeft, meterLeft: null, version: r.version })),
      ];
      return { asOf: day(0), overdue: items.filter((i) => i.status === "overdue").length, dueSoon: items.filter((i) => i.status === "due_soon").length, items };
    },
    settings: async () => settings,
    saveSettings: async (_id, s) => {
      if (!["L", "gal"].includes(s.volumeUnit)) throw new Error("volume unit must be L or gal");
      settings = s;
      return s;
    },
    costs: async () => ({ from: day(-365), to: day(0), total: "0.00", items: [] }),
    file,
    meter: async (_id, value) => {
      meter = Math.max(meter, Number(value));
      return file();
    },
    addReminder: async (vehicleId, input) => {
      if (!input.interval && !input.meterInterval && !input.nextDate) throw new Error("repeat by time, by meter, or give a due date");
      const r: ServiceReminder = { id: `rem-${++seq + 1}`, vehicleId, registrationNo: "MH15AB0001", task: input.task, schedule: "", interval: input.interval ?? null, meterInterval: input.meterInterval ?? null, dueSoonDays: null, dueSoonMeter: null, lastDate: day(0), lastMeter: meter, snoozedUntil: null, state: state(null, input.meterInterval ? meter + input.meterInterval : null, meter), compliance: null, history: [], active: true, version: 1 };
      reminders.push(r);
      return r;
    },
    updateReminder: async (id, _v, input) => {
      const r = reminders.find((x) => x.id === id)!;
      Object.assign(r, { task: input.task, active: input.active ?? true, version: r.version + 1 });
      return r;
    },
    snoozeReminder: async (id, until) => {
      const r = reminders.find((x) => x.id === id)!;
      r.snoozedUntil = until;
      if (until && r.state.status !== "upcoming") r.state = { ...r.state, status: "snoozed" };
      return r;
    },
    addRenewal: async (vehicleId, input) => {
      const r: Renewal = { id: `ren-${++seq + 1}`, vehicleId, registrationNo: "MH15AB0001", renewalType: input.renewalType, label: input.label ?? null, dueDate: input.dueDate, repeat: input.repeat ?? null, snoozedUntil: null, status: "upcoming", daysLeft: null, done: false, history: [], version: 1 };
      renewals.push(r);
      return r;
    },
    completeRenewal: async (id, _v, body) => {
      const r = renewals.find((x) => x.id === id)!;
      r.history.push({ date: body.date ?? day(0), dueDate: r.dueDate, cost: body.cost ? Number(body.cost) * 100 : null });
      if (body.nextDue) r.dueDate = body.nextDue;
      else if (r.repeat && r.dueDate) r.dueDate = `${Number(r.dueDate.slice(0, 4)) + (r.repeat.unit === "years" ? r.repeat.every : 0)}${r.dueDate.slice(4)}`;
      else r.done = true;
      r.status = "upcoming";
      r.version += 1;
      return r;
    },
    snoozeRenewal: async (id, until) => {
      const r = renewals.find((x) => x.id === id)!;
      r.snoozedUntil = until;
      return r;
    },
    addService: async (_id, input) => {
      services.unshift({ id: `svc-${++seq}`, servicedOn: input.servicedOn ?? day(0), meter: input.meter ?? null, description: input.description, vendor: input.vendor ?? null, amount: input.amount, reminderIds: input.reminderIds });
      for (const rid of input.reminderIds) {
        const r = reminders.find((x) => x.id === rid);
        if (r) {
          r.history.push({ date: day(0), meter: input.meter ? Number(input.meter) : null, entryId: null, onTime: r.state.status !== "overdue" });
          r.lastDate = day(0);
          r.state = state(null, input.meter && r.meterInterval ? Number(input.meter) + r.meterInterval : null, meter);
        }
      }
      if (input.meter) meter = Math.max(meter, Number(input.meter));
      return file();
    },
    fuel: () => log(),
    addFuel: async (_id, input) => {
      fuel.unshift({ id: `fuel-${++seq}`, filledOn: input.filledOn ?? day(0), meter: input.meter ? Number(input.meter) : null, volume: Number(input.volume), total: input.total ?? String(Number(input.volume) * Number(input.price ?? 0)), unitPrice: null, partial: !!input.partial, reset: !!input.reset, kind: input.kind ?? "fuel", derived: { usage: null, economy: null, costPerMeter: null } });
      return log(Number(input.volume) > 52.5 ? ["The volume is more than this vehicle's tank holds (50.0)."] : []);
    },
    addExpense: async () => ({ id: `exp-${++seq}` }),
  };
}
