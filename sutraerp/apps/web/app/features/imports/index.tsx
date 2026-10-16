/**
 * Part 2 · S4-ui-import — Admin → Data import (`/data-import/:tab?`), built from the
 * `@sutra/ui` prototype components only (AreaHeader via AreaFrame, BentoTile, Stepper,
 * FormSelect, FileField, DataTable, StatCells, ApprovalCard, Modal).
 *
 *   Import     entity picker + blank template (CSV / XLSX) → upload (size limit shown, 413 and
 *              too-big files explained) → column mapping (suggested by the server, a saved
 *              profile, "save as profile") → dry run (counts, row errors, error-CSV download)
 *              → commit — or, for entities that need it (opening balances), "request approval".
 *   Approvals  imports waiting for a second person: Approve / Reject only for holders of
 *              `platform.import.approve` who are NOT the job's uploader or commit requester.
 *   History    the institution's jobs (open one to resume it).
 *   Export     each entity in its import columns (CSV / XLSX, formula-injection safe server-side).
 *
 * The API enforces every rule shown here (permissions, maker ≠ checker, state machine); the
 * screen only decides what to offer.
 */
import { ArrowRight, Download, FileSpreadsheet, Play, Upload } from "lucide-react";
import {
  ApprovalCard,
  BentoTile,
  Button,
  DataTable,
  EmptyState,
  FileField,
  FilterChips,
  FormGrid,
  FormSelect,
  Modal,
  StatCells,
  StatusBadge,
  Stepper,
  TextArea,
  TextField,
  formatDateTime,
  formatINR,
  formatNumber,
  humanize,
  plural,
} from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState, type FormEvent } from "react";
import { useSearchParams } from "react-router";
import { useSession, type ClientSession } from "../../lib/auth";
import { can } from "../../lib/permissions";
import { NoInstitution } from "../institution/forms";
import { problemText, useCommand } from "../institution/hooks";
import type { Institution } from "../institution/types";
import {
  MAX_IMPORT_FILE_BYTES,
  MAX_IMPORT_ROWS,
  fileBytes,
  formatBytes,
  saveFile,
  toBase64,
  type FileFormat,
  type ImportEntity,
  type ImportJob,
  type ImportJobRow,
  type OnExisting,
} from "./api";
import { ikeys, useImportApi } from "./hooks";

export const IMPORT_TABS = [
  { key: "import", label: "Import" },
  { key: "approvals", label: "Approvals" },
  { key: "history", label: "History" },
  { key: "export", label: "Export" },
] as const;
export type ImportTabKey = (typeof IMPORT_TABS)[number]["key"];

/** Tabs the session may use: import / history / export need `platform.import.manage`, approvals `platform.import.approve`. */
export function importTabsFor(session: Pick<ClientSession, "apiRoles" | "role"> | null): ImportTabKey[] {
  const manage = can(session, "platform.import.manage");
  const approve = can(session, "platform.import.approve");
  return IMPORT_TABS.map((t) => t.key).filter((k) => (k === "approvals" ? approve : manage));
}

export const STATUS_LABELS: Record<string, string> = {
  uploaded: "Map columns",
  validated: "Ready to commit",
  invalid: "Has errors",
  pending_approval: "Waiting for approval",
  committing: "Committing",
  committed: "Committed",
  failed: "Failed",
  rejected: "Rejected",
};

/** True when the session is the job's maker (uploader or commit requester) — never its checker. */
export function isMaker(job: Pick<ImportJob, "createdBy" | "commitRequestedBy">, subject: string | undefined): boolean {
  if (!subject) return true; // unknown principal: never offer the decision
  return job.createdBy === subject || job.commitRequestedBy === subject;
}

/** Offer Approve / Reject: a pending job, the approve permission, and not the maker. */
export function canDecide(job: ImportJob, session: Pick<ClientSession, "apiRoles" | "role" | "subject">): boolean {
  return job.status === "pending_approval" && can(session, "platform.import.approve") && !isMaker(job, session.subject);
}

/** Canonical columns marked required that no header maps to. */
export function missingRequired(entity: ImportEntity | undefined, mapping: Record<string, string>): string[] {
  if (!entity) return [];
  const mapped = new Set(Object.values(mapping));
  return entity.columns.filter((c) => c.required && !mapped.has(c.name)).map((c) => c.label);
}

const STEPS = ["Upload", "Map columns", "Dry run", "Commit"];
function stepOf(job: ImportJob | undefined): number {
  if (!job) return 0;
  switch (job.status) {
    case "uploaded":
      return 1;
    case "validated":
    case "invalid":
      return 2;
    default:
      return 4;
  }
}

/** The screen body (inside `AreaFrame`): one tab of Data import for `institution`. */
export function DataImportTab({ tab, institution }: { tab: ImportTabKey; institution: Institution | null }) {
  const session = useSession();
  const allowed = importTabsFor(session);
  if (!allowed.includes(tab)) {
    return (
      <BentoTile span={12} data-import-denied="">
        <EmptyState title="Data import isn’t available">Importing records needs the data import permission. Ask an administrator.</EmptyState>
      </BentoTile>
    );
  }
  if (!institution)
    return (
      <BentoTile span={12} title="Data import">
        <NoInstitution />
      </BentoTile>
    );
  switch (tab) {
    case "approvals":
      return <ApprovalsTab institution={institution} />;
    case "history":
      return <HistoryTab institution={institution} />;
    case "export":
      return <ExportTab institution={institution} />;
    default:
      return <ImportTab institution={institution} />;
  }
}

// ------------------------------------------------------------------ import

function ImportTab({ institution }: { institution: Institution }) {
  const api = useImportApi();
  const [params, setParams] = useSearchParams();
  const jobId = params.get("job");
  const entities = useQuery({ queryKey: ikeys.entities(), queryFn: () => api.entities() });
  const job = useQuery({ queryKey: ikeys.job(jobId ?? "-"), queryFn: () => api.job(jobId!), enabled: !!jobId });
  const openJob = (id: string | null) => {
    const next = new URLSearchParams(params);
    if (id) next.set("job", id);
    else next.delete("job");
    setParams(next, { replace: false });
  };
  const list = entities.data ?? [];
  const current = job.data;
  const entity = list.find((e) => e.entity === current?.entity);
  return (
    <>
      {!current ? (
        <UploadTile entities={list} loading={entities.isLoading} institution={institution} onUploaded={(j) => openJob(j.id)} />
      ) : (
        <>
          <BentoTile span={12} title={`${entity?.label ?? humanize(current.entity)} · ${current.fileName}`} action={<StatusBadge status={current.status} labels={STATUS_LABELS} />} data-import-job={current.id} data-import-status={current.status}>
            <Stepper steps={STEPS} current={stepOf(current)} />
            <p className="sb-muted" style={{ margin: "10px 0 0", fontSize: 13 }}>
              {plural(current.summary.rowsInFile, "row")} · {current.headers.length} columns · {formatBytes(current.fileBytes)} · uploaded {formatDateTime(current.createdAt)}
              {" · "}
              <button type="button" className="sb-link" style={{ background: "none", border: 0, padding: 0, cursor: "pointer", font: "inherit" }} data-import-new onClick={() => openJob(null)}>
                Start another import
              </button>
            </p>
          </BentoTile>
          {(current.status === "uploaded" || current.status === "validated" || current.status === "invalid") && <MappingTile job={current} entity={entity} />}
          {(current.status === "validated" || current.status === "invalid") && <DryRunTile job={current} />}
          {current.status === "pending_approval" && <PendingTile job={current} entity={entity} />}
          {(current.status === "committed" || current.status === "failed" || current.status === "rejected" || current.status === "committing") && <ReconciliationTile job={current} />}
        </>
      )}
    </>
  );
}

function UploadTile({ entities, loading, institution, onUploaded }: { entities: ImportEntity[]; loading: boolean; institution: Institution; onUploaded: (job: ImportJob) => void }) {
  const api = useImportApi();
  const { run, busy, dialog } = useCommand();
  const usable = entities.filter((e) => e.allowed);
  const [entityKey, setEntityKey] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [profileId, setProfileId] = useState("");
  const [error, setError] = useState<string | null>(null);
  const key = entityKey || usable[0]?.entity || "";
  const entity = entities.find((e) => e.entity === key);
  const profiles = useQuery({ queryKey: ikeys.profiles(key), queryFn: () => api.profiles(key), enabled: !!key });

  const download = (format: FileFormat) => run(async () => saveFile(await api.template(key, format)), `Template downloaded (${format.toUpperCase()})`);
  const pick = (f: File | null) => {
    setError(null);
    setFile(f);
    if (f && f.size > MAX_IMPORT_FILE_BYTES) setError(`${f.name} is ${formatBytes(f.size)}. Files up to ${formatBytes(MAX_IMPORT_FILE_BYTES)} can be imported; split it into several files.`);
    else if (f && !/\.(csv|xlsx)$/i.test(f.name)) setError("Choose a .csv or .xlsx file.");
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!file || !entity || error) return;
    setError(null);
    try {
      const contentBase64 = toBase64(await fileBytes(file));
      const job = await run(
        () => api.upload({ entity: entity.entity, institutionId: institution.id, fileName: file.name, contentBase64, mappingProfileId: profileId || null }),
        (j) => `${j.fileName} uploaded · ${plural(j.summary.rowsInFile, "row")}`,
        { rethrow: true },
      );
      if (job) onUploaded(job);
    } catch (x) {
      setError(problemText(x));
    }
  };
  if (loading) return <BentoTile span={12} title="Import data">Loading…</BentoTile>;
  if (!usable.length)
    return (
      <BentoTile span={12} title="Import data">
        <EmptyState title="Nothing you can import">Each kind of record also needs its own manage permission.</EmptyState>
      </BentoTile>
    );
  return (
    <>
      <BentoTile span={7} title="1 · Choose what to import">
        <FormGrid columns={1}>
          <FormSelect
            label="Records"
            name="entity"
            value={key}
            onChange={(e) => {
              setEntityKey(e.target.value);
              setProfileId("");
            }}
            options={entities.map((x) => ({ value: x.entity, label: x.allowed ? x.label : `${x.label} (no access)`, disabled: !x.allowed }))}
          />
        </FormGrid>
        {entity && (
          <div data-import-columns={entity.entity} style={{ marginTop: 12, fontSize: 13 }}>
            <p style={{ margin: "0 0 6px" }}>
              Matched on <b>{entity.naturalKey.map((k) => entity.columns.find((c) => c.name === k)?.label ?? k).join(" + ")}</b>
              {entity.requiresApproval ? " · a second person approves the commit" : ""}
              {entity.supportsUpdate ? " · existing records can be updated" : " · existing records are skipped"}
            </p>
            <p className="sb-muted" style={{ margin: 0 }}>
              Columns: {entity.columns.map((c) => `${c.label}${c.required ? " *" : ""}`).join(", ")}
            </p>
          </div>
        )}
        <div style={{ display: "flex", gap: 8, marginTop: 14, flexWrap: "wrap" }}>
          <Button variant="ghost" size="sm" icon={<Download strokeWidth={1.75} aria-hidden="true" />} iconPosition="start" disabled={!entity || busy} onClick={() => void download("csv")} data-import-template="csv">
            CSV template
          </Button>
          <Button variant="ghost" size="sm" icon={<FileSpreadsheet strokeWidth={1.75} aria-hidden="true" />} iconPosition="start" disabled={!entity || busy} onClick={() => void download("xlsx")} data-import-template="xlsx">
            Excel template
          </Button>
        </div>
      </BentoTile>
      <BentoTile span={5} title="2 · Upload the file">
        <form onSubmit={(e) => void submit(e)} data-form="import-upload">
          <FileField label="Choose a CSV or Excel file" name="file" accept=".csv,.xlsx,text/csv,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" fileName={file?.name} hint={`CSV or XLSX · up to ${formatBytes(MAX_IMPORT_FILE_BYTES)} · ${formatNumber(MAX_IMPORT_ROWS)} rows`} onFile={pick} />
          {(profiles.data ?? []).length > 0 && (
            <div style={{ marginTop: 12 }}>
              <FormSelect label="Saved mapping (optional)" name="profile" value={profileId} onChange={(e) => setProfileId(e.target.value)} placeholder="Suggest from the headers" options={(profiles.data ?? []).map((p) => ({ value: p.id, label: p.name }))} />
            </div>
          )}
          {error && (
            <p className="sb-field-error" role="alert" data-import-error style={{ marginTop: 10 }}>
              {error}
            </p>
          )}
          <div style={{ marginTop: 14 }}>
            <Button type="submit" disabled={!file || !!error || busy} icon={<Upload strokeWidth={1.75} aria-hidden="true" />} data-import-upload>
              Upload
            </Button>
          </div>
        </form>
      </BentoTile>
      {dialog}
    </>
  );
}

function MappingTile({ job, entity }: { job: ImportJob; entity: ImportEntity | undefined }) {
  const api = useImportApi();
  const { run, busy, dialog } = useCommand();
  const profiles = useQuery({ queryKey: ikeys.profiles(job.entity), queryFn: () => api.profiles(job.entity) });
  const [mapping, setMapping] = useState<Record<string, string>>(job.mapping);
  const [onExisting, setOnExisting] = useState<OnExisting>(job.onExisting);
  const [profileName, setProfileName] = useState("");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    setMapping(job.mapping);
    setOnExisting(job.onExisting);
  }, [job.id, job.version, job.mapping, job.onExisting]);
  const missing = missingRequired(entity, mapping);
  const columns = entity?.columns ?? [];
  const applyProfile = (id: string) => {
    const p = profiles.data?.find((x) => x.id === id);
    if (!p) return;
    setMapping(Object.fromEntries(Object.entries(p.mapping).filter(([h]) => job.headers.includes(h))));
  };
  const dryRun = async () => {
    setError(null);
    try {
      await run(
        async () => {
          await api.setMapping(job.id, { mapping, onExisting, saveAsProfile: profileName.trim() || null });
          return api.dryRun(job.id);
        },
        (j) => (j.actions.error ? `Dry run: ${plural(j.actions.error, "row")} with errors` : `Dry run passed · ${plural(j.actions.create + j.actions.update, "row")} to import`),
        { rethrow: true },
      );
      setProfileName("");
    } catch (x) {
      setError(problemText(x));
    }
  };
  return (
    <BentoTile span={12} title="Column mapping" data-import-mapping="">
      <FormGrid columns={3}>
        {(profiles.data ?? []).length > 0 && (
          <FormSelect label="Apply a saved mapping" name="apply-profile" value="" placeholder="Choose a profile" onChange={(e) => applyProfile(e.target.value)} options={(profiles.data ?? []).map((p) => ({ value: p.id, label: p.name }))} />
        )}
        {entity?.supportsUpdate && (
          <FormSelect
            label="Records that already exist"
            name="onExisting"
            value={onExisting}
            onChange={(e) => setOnExisting(e.target.value as OnExisting)}
            options={[
              { value: "skip", label: "Skip them" },
              { value: "update", label: "Update them" },
            ]}
          />
        )}
        <TextField label="Save this mapping as (optional)" name="saveAsProfile" value={profileName} onChange={(e) => setProfileName(e.target.value)} placeholder="For example, Legacy SIS export" />
      </FormGrid>
      <div style={{ marginTop: 14 }}>
        <DataTable
          caption="Column mapping"
          rows={job.headers.filter((h) => h !== "")}
          rowKey={(h) => h}
          columns={[
            { key: "header", header: "Column in your file", render: (h) => <b data-map-header={h}>{h}</b> },
            {
              key: "target",
              header: "Imports into",
              render: (h) => (
                <FormSelect
                  label={`Imports into for ${h}`}
                  hideLabel
                  name={`map:${h}`}
                  value={mapping[h] ?? ""}
                  placeholder="Don’t import"
                  onChange={(e) => {
                    const v = e.target.value;
                    setMapping((m) => {
                      const next = { ...m };
                      if (v) next[h] = v;
                      else delete next[h];
                      return next;
                    });
                  }}
                  options={columns.map((c) => ({ value: c.name, label: `${c.label}${c.required ? " *" : ""}` }))}
                />
              ),
            },
            { key: "sample", header: "First value", render: (h) => <SampleCell job={job} header={h} />, muted: true },
          ]}
        />
      </div>
      {missing.length > 0 && (
        <p className="sb-field-error" role="alert" data-import-missing style={{ marginTop: 10 }}>
          Required columns not mapped: {missing.join(", ")}. The dry run will report every row.
        </p>
      )}
      {error && (
        <p className="sb-field-error" role="alert" data-import-error style={{ marginTop: 10 }}>
          {error}
        </p>
      )}
      <div style={{ marginTop: 14 }}>
        <Button disabled={busy} onClick={() => void dryRun()} icon={<Play strokeWidth={1.75} aria-hidden="true" />} data-import-dry-run>
          {job.status === "uploaded" ? "Save mapping and dry run" : "Run the dry run again"}
        </Button>
      </div>
      {dialog}
    </BentoTile>
  );
}

function SampleCell({ job, header }: { job: ImportJob; header: string }) {
  const api = useImportApi();
  const rows = useQuery({ queryKey: ikeys.rows(job.id, false), queryFn: () => api.rows(job.id, false) });
  const v = rows.data?.[0]?.cells[header];
  return <>{v ? v : "—"}</>;
}

function errorText(r: ImportJobRow): string {
  return r.errors.map((e) => `${e.field ? `${e.field}: ` : ""}${e.message}`).join(" · ");
}

function DryRunTile({ job }: { job: ImportJob }) {
  const api = useImportApi();
  const { run, busy, dialog } = useCommand();
  const hasErrors = job.actions.error > 0;
  const [filter, setFilter] = useState<"errors" | "all">(hasErrors ? "errors" : "all");
  useEffect(() => setFilter(hasErrors ? "errors" : "all"), [hasErrors, job.version]);
  const errorsOnly = filter === "errors";
  const rows = useQuery({ queryKey: ikeys.rows(job.id, errorsOnly), queryFn: () => api.rows(job.id, errorsOnly) });
  const toImport = job.actions.create + job.actions.update;
  const commitLabel = job.requiresApproval ? "Request approval" : `Commit ${plural(toImport, "row")}`;
  return (
    <>
      <BentoTile span={12} title="Dry run" action={<span className="sb-muted" style={{ fontSize: 13 }}>{job.validatedMs != null ? `${formatNumber(job.validatedMs)} ms` : ""}</span>} data-import-dryrun={job.status}>
        <StatCells
          columns={4}
          cells={[
            { label: "To create", value: formatNumber(job.actions.create), kind: job.actions.create ? "done" : undefined },
            { label: "To update", value: formatNumber(job.actions.update) },
            { label: "Already there (skipped)", value: formatNumber(job.actions.skip) },
            { label: "Rows with errors", value: formatNumber(job.actions.error), kind: hasErrors ? "attention" : undefined },
          ]}
        />
        {job.summary.fileTotal != null && (
          <p style={{ margin: "12px 0 0", fontSize: 13 }} data-import-file-total={job.summary.fileTotal}>
            File total {formatINR(job.summary.fileTotal)}
          </p>
        )}
        <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap", marginTop: 14 }}>
          <FilterChips
            label="Rows"
            value={filter}
            onChange={(v) => setFilter(v as "errors" | "all")}
            options={[
              { id: "errors", label: "Errors", count: job.actions.error },
              { id: "all", label: "All rows", count: job.summary.rowsInFile },
            ]}
          />
          {hasErrors && (
            <Button variant="ghost" size="sm" icon={<Download strokeWidth={1.75} aria-hidden="true" />} iconPosition="start" disabled={busy} onClick={() => void run(async () => saveFile(await api.errorReport(job.id)), "Error report downloaded")} data-import-error-csv>
              Download error report (CSV)
            </Button>
          )}
        </div>
        <div style={{ marginTop: 12 }}>
          <DataTable
            caption="Dry-run rows"
            rows={rows.data ?? []}
            rowKey={(r) => String(r.line)}
            empty={<EmptyState title={errorsOnly ? "No row errors" : "No rows"} compact />}
            columns={[
              { key: "line", header: "Line", render: (r) => <span data-import-row={r.line}>{r.line}</span>, muted: true },
              { key: "key", header: "Record", render: (r) => r.naturalKey ?? "—" },
              { key: "action", header: "Result", render: (r) => <StatusBadge status={r.action ?? "pending"} labels={{ create: "Create", update: "Update", skip: "Skip", error: "Error", pending: "Not checked" }} kind={r.action === "error" ? "attention" : r.action === "create" || r.action === "update" ? "done" : "neutral"} /> },
              { key: "errors", header: "Problems", render: (r) => (r.errors.length ? <span data-import-row-error={r.line}>{errorText(r)}</span> : "—") },
            ]}
          />
        </div>
        <div style={{ display: "flex", gap: 10, alignItems: "center", marginTop: 14, flexWrap: "wrap" }}>
          <Button disabled={busy || job.status !== "validated" || toImport === 0} onClick={() => void run(() => api.commit(job.id), (j) => (j.status === "pending_approval" ? "Sent for approval" : j.status === "committed" ? `Imported · ${plural(j.summary.created + j.summary.updated, "record")}` : `Import ${humanize(j.status)}`))} icon={<ArrowRight strokeWidth={1.75} aria-hidden="true" />} data-import-commit>
            {commitLabel}
          </Button>
          <span className="sb-muted" style={{ fontSize: 13 }}>
            {job.status === "invalid" ? "Fix the file (or the mapping) and upload it again — nothing is imported while a row has errors." : toImport === 0 ? "Nothing new to import." : job.requiresApproval ? "A second person with import approval commits it." : `Committed in batches of ${formatNumber(job.batchSize)}.`}
          </span>
        </div>
      </BentoTile>
      {dialog}
    </>
  );
}

function useDecision() {
  const api = useImportApi();
  const { run, busy, dialog } = useCommand();
  const [rejecting, setRejecting] = useState<ImportJob | null>(null);
  const [reason, setReason] = useState("");
  const approve = (job: ImportJob) => run(() => api.decide(job.id, true), "Approved · import committed");
  const reject = async () => {
    if (!rejecting || !reason.trim()) return;
    const r = await run(() => api.decide(rejecting.id, false, reason.trim()), "Import rejected");
    if (r) {
      setRejecting(null);
      setReason("");
    }
  };
  const modal = (
    <Modal open={!!rejecting} onClose={() => setRejecting(null)} label="Reject import">
      <form
        data-form="import-reject"
        onSubmit={(e) => {
          e.preventDefault();
          void reject();
        }}
      >
        <h3 style={{ marginTop: 0 }}>Reject {rejecting?.fileName}</h3>
        <TextArea label="Reason" name="reason" required value={reason} onChange={(e) => setReason(e.target.value)} placeholder="Tell the uploader what to correct" />
        <div style={{ display: "flex", gap: 8, marginTop: 14 }}>
          <Button type="submit" disabled={busy || !reason.trim()}>
            Reject
          </Button>
          <Button variant="ghost" onClick={() => setRejecting(null)}>
            Cancel
          </Button>
        </div>
      </form>
    </Modal>
  );
  return {
    approve,
    startReject: setRejecting,
    busy,
    dialogs: (
      <>
        {dialog}
        {modal}
      </>
    ),
  };
}

function PendingTile({ job, entity }: { job: ImportJob; entity: ImportEntity | undefined }) {
  const session = useSession();
  const { approve, startReject, busy, dialogs } = useDecision();
  const decide = canDecide(job, session);
  return (
    <BentoTile span={12} title="Approval" data-import-pending="">
      <ApprovalCard
        icon={FileSpreadsheet}
        tone="violet"
        title={`${entity?.label ?? humanize(job.entity)} · ${job.fileName}`}
        meta={`${plural(job.actions.create + job.actions.update, "row")} to import${job.summary.fileTotal != null ? ` · ${formatINR(job.summary.fileTotal)}` : ""} · requested ${formatDateTime(job.updatedAt)}`}
        onApprove={decide ? () => void approve(job) : undefined}
        onReject={decide ? () => startReject(job) : undefined}
        busy={busy}
        data-import-approval={job.id}
      />
      {!decide && (
        <p className="sb-muted" style={{ margin: "10px 0 0", fontSize: 13 }} data-import-awaiting="">
          {isMaker(job, session.subject) ? "You raised this import, so another person with import approval decides it." : "Waiting for a person with import approval."} Nothing is posted until it is approved.
        </p>
      )}
      {dialogs}
    </BentoTile>
  );
}

function ReconciliationTile({ job }: { job: ImportJob }) {
  const s = job.summary;
  const money = s.fileTotal != null;
  return (
    <BentoTile span={12} title="Reconciliation" action={<StatusBadge status={job.status} labels={STATUS_LABELS} />} data-import-reconciliation={job.status}>
      <StatCells
        columns={4}
        cells={[
          { label: "Rows in file", value: formatNumber(s.rowsInFile) },
          { label: "Created", value: formatNumber(s.created), kind: s.created ? "done" : undefined },
          { label: "Updated", value: formatNumber(s.updated) },
          { label: "Skipped", value: formatNumber(s.skipped), sub: s.existingInDatabase ? `${formatNumber(s.existingInDatabase)} already in the database` : undefined },
          { label: "Errors", value: formatNumber(s.errors), kind: s.errors ? "attention" : undefined },
          { label: "Duplicates in file", value: formatNumber(s.duplicatesInFile) },
          { label: "Batches committed", value: formatNumber(s.batchesCommitted) },
          { label: "Commit time", value: job.committedMs != null ? `${formatNumber(job.committedMs)} ms` : "—" },
        ]}
      />
      {money && (
        <div style={{ marginTop: 12 }} data-import-reconciled={String(!!s.reconciled)}>
          <StatCells
            columns={4}
            cells={[
              { label: "File total", value: formatINR(s.fileTotal) },
              { label: "Committed total", value: formatINR(s.committedTotal) },
              { label: "Ledger total", value: formatINR(s.ledgerTotal) },
              { label: "Reconciled", value: s.reconciled ? "Yes" : "No", kind: s.reconciled ? "done" : "attention" },
            ]}
          />
        </div>
      )}
      {job.failureReason && (
        <p className="sb-field-error" role="alert" style={{ marginTop: 10 }}>
          {job.failureReason}
        </p>
      )}
      {job.status === "rejected" && job.decisionReason && (
        <p style={{ marginTop: 10, fontSize: 13 }} data-import-rejection="">
          Rejected: {job.decisionReason}
        </p>
      )}
    </BentoTile>
  );
}

// ------------------------------------------------------------------ approvals / history / export

function useJobs(institution: Institution) {
  const api = useImportApi();
  return useQuery({ queryKey: ikeys.jobs(institution.id), queryFn: () => api.jobs(institution.id) });
}

function ApprovalsTab({ institution }: { institution: Institution }) {
  const session = useSession();
  const jobs = useJobs(institution);
  const { approve, startReject, busy, dialogs } = useDecision();
  const pending = (jobs.data ?? []).filter((j) => j.status === "pending_approval");
  return (
    <BentoTile span={12} title="Imports waiting for approval" action={<span className="sb-muted" style={{ fontSize: 13 }}>{`${pending.length} pending`}</span>} data-import-approvals="">
      {jobs.isLoading ? (
        "Loading…"
      ) : pending.length === 0 ? (
        <EmptyState title="Nothing to approve" compact>
          Imports that need a second person (opening fee balances) appear here.
        </EmptyState>
      ) : (
        <div style={{ display: "grid", gap: 10 }}>
          {pending.map((j) => {
            const decide = canDecide(j, session);
            return (
              <ApprovalCard
                key={j.id}
                icon={FileSpreadsheet}
                tone="violet"
                title={`${humanize(j.entity)} · ${j.fileName}`}
                meta={`${plural(j.actions.create + j.actions.update, "row")}${j.summary.fileTotal != null ? ` · ${formatINR(j.summary.fileTotal)}` : ""}${decide ? "" : isMaker(j, session.subject) ? " · raised by you — another approver decides" : ""}`}
                onApprove={decide ? () => void approve(j) : undefined}
                onReject={decide ? () => startReject(j) : undefined}
                busy={busy}
                data-import-approval={j.id}
              />
            );
          })}
        </div>
      )}
      {dialogs}
    </BentoTile>
  );
}

function HistoryTab({ institution }: { institution: Institution }) {
  const jobs = useJobs(institution);
  const [, setParams] = useSearchParams();
  const list = jobs.data ?? [];
  const open = (id: string) => setParams(new URLSearchParams({ job: id }));
  return (
    <BentoTile span={12} title="Import history" action={<span className="sb-muted" style={{ fontSize: 13 }}>{`${list.length} jobs`}</span>} data-import-history="">
      <DataTable
        caption="Import jobs"
        rows={list}
        rowKey={(j) => j.id}
        empty={<EmptyState title="No imports yet" compact>Upload a file on the Import tab.</EmptyState>}
        columns={[
          { key: "file", header: "File", render: (j) => <b data-import-history-row={j.fileName}>{j.fileName}</b> },
          { key: "entity", header: "Records", render: (j) => humanize(j.entity) },
          { key: "rows", header: "Rows", render: (j) => formatNumber(j.summary.rowsInFile), muted: true },
          { key: "result", header: "Result", render: (j) => (j.status === "committed" ? `${formatNumber(j.summary.created + j.summary.updated)} imported · ${formatNumber(j.summary.skipped)} skipped` : j.actions.error ? `${formatNumber(j.actions.error)} errors` : "—"), muted: true },
          { key: "when", header: "Uploaded", render: (j) => formatDateTime(j.createdAt), muted: true },
          { key: "status", header: "Status", render: (j) => <StatusBadge status={j.status} labels={STATUS_LABELS} /> },
          {
            key: "open",
            header: "",
            render: (j) => (
              <a href={`/data-import?job=${encodeURIComponent(j.id)}`} className="sb-link" data-action="import-open" onClick={(e) => {
                e.preventDefault();
                open(j.id);
              }}>
                Open
              </a>
            ),
          },
        ]}
      />
    </BentoTile>
  );
}

function ExportTab({ institution }: { institution: Institution }) {
  const api = useImportApi();
  const { run, busy, dialog } = useCommand();
  const entities = useQuery({ queryKey: ikeys.entities(), queryFn: () => api.entities() });
  const list = useMemo(() => (entities.data ?? []).filter((e) => e.allowed), [entities.data]);
  const download = (entity: string, format: FileFormat) => run(async () => saveFile(await api.exportFile(entity, institution.id, format)), `Export downloaded (${format.toUpperCase()})`);
  return (
    <BentoTile span={12} title="Export" data-import-export="">
      <p className="sb-muted" style={{ margin: "0 0 12px", fontSize: 13 }}>
        Records in their import columns — edit and re-import them, or keep them as a backup. Cells a spreadsheet would run as a formula are exported as text.
      </p>
      <DataTable
        caption="Exports"
        rows={list}
        rowKey={(e) => e.entity}
        empty={<EmptyState title={entities.isLoading ? "Loading…" : "Nothing you can export"} compact />}
        columns={[
          { key: "label", header: "Records", render: (e) => <b>{e.label}</b> },
          { key: "cols", header: "Columns", render: (e) => formatNumber(e.columns.length), muted: true },
          {
            key: "dl",
            header: "",
            render: (e) => (
              <span style={{ display: "inline-flex", gap: 6 }}>
                <Button variant="ghost" size="sm" disabled={busy} onClick={() => void download(e.entity, "csv")} data-import-export-csv={e.entity}>
                  CSV
                </Button>
                <Button variant="ghost" size="sm" disabled={busy} onClick={() => void download(e.entity, "xlsx")} data-import-export-xlsx={e.entity}>
                  Excel
                </Button>
              </span>
            ),
          },
        ]}
      />
      {dialog}
    </BentoTile>
  );
}
