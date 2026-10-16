/**
 * In-memory Data import API (mock mode + component tests). Follows the server's state machine
 * (`uploaded → validated | invalid → committed`, approval entities `→ pending_approval →
 * committed | rejected`), its CSV header rules and the maker ≠ checker rule; validation is a
 * small subset (required cells, dates, e-mails, in-file duplicates). Never authoritative.
 */
import { ApiProblem, type components } from "@sutra/api-client";
import type { DownloadedFile, FileFormat, ImportApi, ImportEntity, ImportJob, ImportJobRow, ImportMappingUpdate, ImportUpload, MappingProfile } from "./api";

type S = components["schemas"];

const col = (name: string, label: string, kind: S["ColumnKind"], required = false, example = ""): S["ImportColumnView"] => ({ name, label, kind, required, example, help: "", choices: [] });

export const MOCK_ENTITIES: ImportEntity[] = [
  {
    entity: "students",
    label: "Students (with a guardian)",
    owner: "student",
    permission: "student.manage",
    allowed: true,
    requiresApproval: false,
    supportsUpdate: false,
    naturalKey: ["student_number"],
    columns: [
      col("student_number", "Student number", "text", true, "S2023/0001"),
      col("given_name", "Given name", "text", true, "Aarav"),
      col("family_name", "Family name", "text", false, "Desai"),
      col("date_of_birth", "Date of birth", "date", false, "2005-04-12"),
      col("email", "E-mail", "email", false, "aarav@example.org"),
      col("programme_code", "Programme code", "code", true, "BTECH-CSE"),
    ],
  },
  {
    entity: "opening_balances",
    label: "Opening fee balances",
    owner: "finance",
    permission: "finance.manage",
    allowed: true,
    requiresApproval: true,
    supportsUpdate: false,
    naturalKey: ["student_number"],
    amountColumn: "amount",
    columns: [col("student_number", "Student number", "text", true, "S2023/0001"), col("amount", "Amount", "decimal", true, "12500.00"), col("due_date", "Due date", "date", false, "2026-12-31")],
  },
];

const clone = <T,>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const problem = (status: number, errorCode: string, detail: string) => new ApiProblem({ type: "about:blank", title: detail, status, errorCode, detail });

function parseCsv(text: string): string[][] {
  return text
    .replace(/^\uFEFF/, "")
    .split(/\r?\n/)
    .filter((l) => l.trim() !== "")
    .map((l) => l.split(",").map((c) => c.trim().replace(/^"(.*)"$/, "$1")));
}

function suggest(headers: string[], entity: ImportEntity): Record<string, string> {
  const norm = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, "");
  const out: Record<string, string> = {};
  for (const h of headers) {
    const c = entity.columns.find((x) => norm(x.name) === norm(h) || norm(x.label) === norm(h));
    if (c) out[h] = c.name;
  }
  return out;
}

const emptySummary = (rowsInFile: number): S["Reconciliation"] => ({ rowsInFile, created: 0, updated: 0, skipped: 0, errors: 0, duplicatesInFile: 0, existingInDatabase: 0, batchesCommitted: 0 });
const counts = (): S["ActionCounts"] => ({ create: 0, update: 0, skip: 0, error: 0, pending: 0 });

interface MockJob {
  job: ImportJob;
  cells: Array<Record<string, string>>;
  rows: ImportJobRow[];
}

export function createMockImportApi(opts: { actor?: string; jobs?: ImportJob[]; rows?: Record<string, ImportJobRow[]> } = {}): ImportApi {
  const actor = opts.actor ?? "dev|admin@sutra.dev";
  const jobs = new Map<string, MockJob>();
  for (const j of opts.jobs ?? []) jobs.set(j.id, { job: clone(j), cells: [], rows: clone(opts.rows?.[j.id] ?? []) });
  const profiles: MappingProfile[] = [{ id: "prof-1", entity: "students", name: "Legacy SIS export", mapping: { "Roll No": "student_number", "First Name": "given_name", Programme: "programme_code" }, version: 1, updatedAt: "2026-10-01T09:00:00Z" }];
  const existing = new Set<string>(["S2020/0001"]);
  let seq = jobs.size;

  const get = (id: string): MockJob => {
    const j = jobs.get(id);
    if (!j) throw problem(404, "not-found", "import job not found");
    return j;
  };
  const entityOf = (name: string): ImportEntity => {
    const e = MOCK_ENTITIES.find((x) => x.entity === name);
    if (!e) throw problem(404, "not-found", `unknown entity ${name}`);
    return e;
  };
  const touch = (m: MockJob, patch: Partial<ImportJob>) => {
    m.job = { ...m.job, ...patch, version: m.job.version + 1, updatedAt: new Date().toISOString() };
    return clone(m.job);
  };
  const file = (name: string, text: string): DownloadedFile => ({ blob: new Blob([text], { type: "text/csv" }), fileName: name });

  return {
    async entities() {
      return clone(MOCK_ENTITIES);
    },
    async profiles(entity) {
      return profiles.filter((p) => p.entity === entity).map((p) => clone(p));
    },
    async jobs(institutionId) {
      return [...jobs.values()].filter((m) => m.job.institutionId === institutionId).map((m) => clone(m.job)).sort((a, b) => b.createdAt.localeCompare(a.createdAt));
    },
    async job(id) {
      return clone(get(id).job);
    },
    async rows(id, errorsOnly) {
      return get(id)
        .rows.filter((r) => !errorsOnly || r.action === "error")
        .map((r) => clone(r));
    },
    async upload(input: ImportUpload) {
      const entity = entityOf(input.entity);
      if (!/\.(csv|xlsx)$/i.test(input.fileName)) throw problem(422, "validation-failed", "the file must be .csv or .xlsx");
      const text = atob(input.contentBase64);
      const [headers = [], ...data] = parseCsv(text);
      if (!headers.length || !data.length) throw problem(422, "validation-failed", "the file has no data rows");
      const id = `job-${++seq}`;
      const now = new Date(Date.now() + seq).toISOString();
      const profile = profiles.find((p) => p.id === input.mappingProfileId);
      const mapping = profile ? Object.fromEntries(Object.entries(profile.mapping).filter(([h]) => headers.includes(h))) : suggest(headers, entity);
      const job: ImportJob = {
        id,
        entity: entity.entity,
        institutionId: input.institutionId,
        jobKey: input.jobKey ?? `${entity.entity}:${input.institutionId}:${id}`,
        fileName: input.fileName,
        fileFormat: input.fileName.toLowerCase().endsWith(".xlsx") ? "xlsx" : "csv",
        fileBytes: text.length,
        fileSha256: id,
        headers,
        mapping,
        mappingProfileId: profile?.id ?? null,
        onExisting: input.onExisting ?? "skip",
        batchSize: input.batchSize ?? 500,
        requiresApproval: entity.requiresApproval,
        status: "uploaded",
        actions: { ...counts(), pending: data.length },
        summary: emptySummary(data.length),
        createdAt: now,
        updatedAt: now,
        createdBy: actor,
        version: 1,
      };
      const cells = data.map((r) => Object.fromEntries(headers.map((h, i) => [h, r[i] ?? ""])));
      jobs.set(id, { job, cells, rows: cells.map((c, i) => ({ line: i + 2, cells: c, committed: false, errors: [] })) });
      return clone(job);
    },
    async setMapping(id, input: ImportMappingUpdate) {
      const m = get(id);
      if (!["uploaded", "validated", "invalid"].includes(m.job.status)) throw problem(409, "conflict", "the job can no longer be changed");
      const entity = entityOf(m.job.entity);
      for (const [h, c] of Object.entries(input.mapping)) {
        if (!m.job.headers.includes(h)) throw problem(422, "validation-failed", `unknown header ${h}`);
        if (!entity.columns.some((x) => x.name === c)) throw problem(422, "validation-failed", `unknown column ${c}`);
      }
      if (input.saveAsProfile?.trim()) {
        const name = input.saveAsProfile.trim();
        const at = profiles.findIndex((p) => p.entity === entity.entity && p.name === name);
        const p: MappingProfile = { id: at >= 0 ? profiles[at]!.id : `prof-${profiles.length + 1}`, entity: entity.entity, name, mapping: { ...input.mapping }, version: 1, updatedAt: new Date().toISOString() };
        if (at >= 0) profiles[at] = p;
        else profiles.push(p);
      }
      m.rows = m.cells.map((c, i) => ({ line: i + 2, cells: c, committed: false, errors: [] }));
      return touch(m, { mapping: { ...input.mapping }, onExisting: input.onExisting ?? m.job.onExisting, status: "uploaded", actions: { ...counts(), pending: m.cells.length }, summary: emptySummary(m.cells.length) });
    },
    async dryRun(id) {
      const m = get(id);
      if (!["uploaded", "validated", "invalid"].includes(m.job.status)) throw problem(409, "conflict", "the job was already committed");
      const entity = entityOf(m.job.entity);
      const byColumn = Object.fromEntries(Object.entries(m.job.mapping).map(([h, c]) => [c, h]));
      const seen = new Set<string>();
      const a = counts();
      const summary = emptySummary(m.cells.length);
      let total = 0;
      m.rows = m.cells.map((cells, i) => {
        const value = (c: string) => (byColumn[c] ? (cells[byColumn[c]!] ?? "").trim() : "");
        const errors: S["FieldViolation"][] = [];
        for (const c of entity.columns) {
          const v = value(c.name);
          if (c.required && !v) errors.push({ field: c.name, code: "required", message: `${c.label} is required` });
          else if (v && c.kind === "date" && !/^\d{4}-\d{2}-\d{2}$/.test(v)) errors.push({ field: c.name, code: "date", message: `${c.label} must be a date (YYYY-MM-DD)` });
          else if (v && c.kind === "email" && !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(v)) errors.push({ field: c.name, code: "email", message: `${c.label} must be an e-mail address` });
          else if (v && c.kind === "decimal" && !/^-?\d+(\.\d+)?$/.test(v.replace(/,/g, ""))) errors.push({ field: c.name, code: "decimal", message: `${c.label} must be a number` });
        }
        const key = entity.naturalKey.map(value).join("|");
        if (key && seen.has(key)) {
          errors.push({ field: entity.naturalKey[0]!, code: "duplicate_in_file", message: `duplicate of an earlier row (${key})` });
          summary.duplicatesInFile++;
        }
        seen.add(key);
        const action: S["RowAction"] = errors.length ? "error" : existing.has(key) ? "skip" : "create";
        if (action === "skip") summary.existingInDatabase++;
        if (action === "create" && entity.amountColumn) total += Number(value(entity.amountColumn).replace(/,/g, ""));
        a[action]++;
        if (action === "error") summary.errors++;
        else if (action === "skip") summary.skipped++;
        else summary.created++;
        return { line: i + 2, cells, errors, action, naturalKey: key || null, committed: false, amount: entity.amountColumn ? value(entity.amountColumn) || null : null };
      });
      if (entity.amountColumn) summary.fileTotal = total.toFixed(2);
      return touch(m, { status: a.error ? "invalid" : "validated", actions: a, summary: { ...summary, created: 0, skipped: 0 }, validatedAt: new Date().toISOString(), validatedMs: 12 });
    },
    async commit(id) {
      const m = get(id);
      if (m.job.status !== "validated") throw problem(409, "conflict", "only a validated job can be committed");
      if (m.job.requiresApproval) return touch(m, { status: "pending_approval", commitRequestedBy: actor });
      return commitNow(m, actor);
    },
    async decide(id, approve, reason) {
      const m = get(id);
      if (m.job.status !== "pending_approval") throw problem(409, "conflict", "nothing to decide");
      if (m.job.createdBy === actor || m.job.commitRequestedBy === actor) throw problem(403, "forbidden", "the maker of the import cannot decide it");
      if (!approve && !reason?.trim()) throw problem(422, "validation-failed", "a rejection needs a reason");
      if (!approve) return touch(m, { status: "rejected", decidedBy: actor, decisionReason: reason ?? null });
      return commitNow(m, actor, true);
    },
    async template(entity, format: FileFormat) {
      const e = entityOf(entity);
      return file(`${entity}-template.${format}`, `${e.columns.map((c) => c.name).join(",")}\n${e.columns.map((c) => c.example).join(",")}\n`);
    },
    async exportFile(entity, _institutionId, format: FileFormat) {
      const e = entityOf(entity);
      return file(`${entity}-export.${format}`, `${e.columns.map((c) => c.name).join(",")}\n`);
    },
    async errorReport(id) {
      const m = get(id);
      const lines = m.rows.filter((r) => r.action === "error").flatMap((r) => r.errors.map((e) => `${r.line},${e.field},${e.code},${e.message}`));
      return file(`import-${id}-errors.csv`, `line,column,code,message\n${lines.join("\n")}\n`);
    },
  };

  function commitNow(m: MockJob, by: string, decided = false): ImportJob {
    const s = { ...m.job.summary };
    let total = 0;
    for (const r of m.rows) {
      if (r.action === "create") {
        r.committed = true;
        r.recordId = `rec-${m.job.id}-${r.line}`;
        if (r.naturalKey) existing.add(r.naturalKey);
        s.created++;
        total += Number((r.amount ?? "0").replace(/,/g, ""));
      } else if (r.action === "skip") s.skipped++;
    }
    s.batchesCommitted = Math.max(1, Math.ceil(m.rows.length / m.job.batchSize));
    if (m.job.summary.fileTotal != null) {
      s.committedTotal = total.toFixed(2);
      s.ledgerTotal = total.toFixed(2);
      s.reconciled = s.ledgerTotal === s.fileTotal;
    }
    return touch(m, { status: "committed", summary: s, committedAt: new Date().toISOString(), committedBy: by, committedMs: 40, ...(decided ? { decidedBy: by } : {}) });
  }
}
