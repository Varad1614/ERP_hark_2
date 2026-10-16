/**
 * Part 2 · S4-ui-import — API layer of Admin → Data import (`/api/v1/platform/imports…`).
 *
 * The HTTP implementation uses the generated, typed client (`@sutra/api-client`, `schema.d.ts`
 * from `openapi/openapi.json`), so a renamed path or field fails `tsc`. CSRF is added by the
 * client middleware; the upload carries an `Idempotency-Key`. Files (templates, exports, the
 * error report) are read as blobs. An in-memory implementation (`./mock`) backs mock mode and
 * the component tests.
 */
import { ApiProblem, isProblem, newIdempotencyKey, type components } from "@sutra/api-client";
import { api as defaultClient } from "../../lib/api";

type S = components["schemas"];
export type ImportEntity = S["ImportEntityView"];
export type ImportColumn = S["ImportColumnView"];
export type ImportJob = S["ImportJob"];
export type ImportJobRow = S["ImportJobRow"];
export type MappingProfile = S["MappingProfile"];
export type JobStatus = S["JobStatus"];
export type OnExisting = S["OnExisting"];
export type FileFormat = S["FileFormat"];
export type ImportUpload = S["ImportUpload"];
export type ImportMappingUpdate = S["ImportMappingUpdate"];

/** Largest file the import engine accepts (decoded; `dataimport/engine.rs` MAX_FILE_BYTES). */
export const MAX_IMPORT_FILE_BYTES = 8 * 1024 * 1024;
/** Most data rows per file (`files.rs` MAX_ROWS). */
export const MAX_IMPORT_ROWS = 20_000;

export interface DownloadedFile {
  blob: Blob;
  fileName: string;
}

export interface ImportApi {
  entities(): Promise<ImportEntity[]>;
  profiles(entity: string): Promise<MappingProfile[]>;
  jobs(institutionId: string): Promise<ImportJob[]>;
  job(id: string): Promise<ImportJob>;
  rows(id: string, errorsOnly: boolean): Promise<ImportJobRow[]>;
  upload(input: ImportUpload): Promise<ImportJob>;
  setMapping(id: string, input: ImportMappingUpdate): Promise<ImportJob>;
  dryRun(id: string): Promise<ImportJob>;
  commit(id: string): Promise<ImportJob>;
  decide(id: string, approve: boolean, reason?: string): Promise<ImportJob>;
  template(entity: string, format: FileFormat): Promise<DownloadedFile>;
  exportFile(entity: string, institutionId: string, format: FileFormat): Promise<DownloadedFile>;
  errorReport(id: string): Promise<DownloadedFile>;
}

/** Problem shown when a proxy / the API refuses the request body as too large (413). */
export function tooLargeProblem(): ApiProblem {
  return new ApiProblem({
    type: "about:blank",
    title: "File too large",
    status: 413,
    errorCode: "payload-too-large",
    detail: `The file is larger than the server accepts. Files up to ${formatBytes(MAX_IMPORT_FILE_BYTES)} can be imported; split a bigger file into several.`,
  } as S["Problem"]);
}

/** `1.5 MiB` / `820 KB` style size. */
export function formatBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(n % (1024 * 1024) === 0 ? 0 : 1)} MiB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} bytes`;
}

/** Base64 of the file's bytes (chunked, so large files do not overflow the call stack). */
export function toBase64(bytes: Uint8Array): string {
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  return btoa(bin);
}

export async function fileBytes(file: Blob): Promise<Uint8Array> {
  if (typeof file.arrayBuffer === "function") return new Uint8Array(await file.arrayBuffer());
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => resolve(new Uint8Array(r.result as ArrayBuffer));
    r.onerror = () => reject(r.error ?? new Error("The file could not be read"));
    r.readAsArrayBuffer(file);
  });
}

type Result<T> = { data?: T; error?: unknown; response: Response };

function check<T>(r: Result<T>): T {
  if (r.response.status === 413) throw tooLargeProblem();
  if (r.error !== undefined) {
    if (isProblem(r.error)) throw new ApiProblem(r.error);
    throw new Error(`Request failed (${r.response.status})`);
  }
  if (r.data === undefined) throw new Error("Empty response");
  return r.data;
}

/** File name from `Content-Disposition` (`attachment; filename="…"`), else `fallback`. */
export function dispositionName(res: Response, fallback: string): string {
  const cd = res.headers.get("content-disposition") ?? "";
  const m = /filename\*?=(?:UTF-8'')?"?([^";]+)"?/i.exec(cd);
  return m?.[1] ? decodeURIComponent(m[1]) : fallback;
}

function file(r: Result<unknown>, fallback: string): DownloadedFile {
  const blob = check(r) as Blob;
  return { blob, fileName: dispositionName(r.response, fallback) };
}

type Client = typeof defaultClient;

export function createHttpImportApi(client: Client = defaultClient): ImportApi {
  return {
    async entities() {
      return check(await client.GET("/api/v1/platform/imports/entities")).items;
    },
    async profiles(entity) {
      return check(await client.GET("/api/v1/platform/imports/mapping-profiles", { params: { query: { entity } } })).items;
    },
    async jobs(institutionId) {
      return check(await client.GET("/api/v1/platform/imports", { params: { query: { institutionId } } })).items;
    },
    async job(id) {
      return check(await client.GET("/api/v1/platform/imports/{id}", { params: { path: { id } } }));
    },
    async rows(id, errorsOnly) {
      return check(await client.GET("/api/v1/platform/imports/{id}/rows", { params: { path: { id }, query: { errorsOnly, limit: 500 } } })).items;
    },
    async upload(input) {
      return check(await client.POST("/api/v1/platform/imports", { params: { header: { "Idempotency-Key": newIdempotencyKey() } }, body: input }));
    },
    async setMapping(id, input) {
      return check(await client.PUT("/api/v1/platform/imports/{id}/mapping", { params: { path: { id } }, body: input }));
    },
    async dryRun(id) {
      return check(await client.POST("/api/v1/platform/imports/{id}/dry-run", { params: { path: { id } } }));
    },
    async commit(id) {
      return check(await client.POST("/api/v1/platform/imports/{id}/commit", { params: { path: { id } } }));
    },
    async decide(id, approve, reason) {
      return check(await client.POST("/api/v1/platform/imports/{id}/decision", { params: { path: { id } }, body: { approve, reason: reason || null } }));
    },
    async template(entity, format) {
      const r = await client.GET("/api/v1/platform/imports/entities/{entity}/template", { params: { path: { entity }, query: { format } }, parseAs: "blob" });
      return file(r, `${entity}-template.${format}`);
    },
    async exportFile(entity, institutionId, format) {
      const r = await client.GET("/api/v1/platform/imports/entities/{entity}/export", { params: { path: { entity }, query: { institutionId, format } }, parseAs: "blob" });
      return file(r, `${entity}-export.${format}`);
    },
    async errorReport(id) {
      const r = await client.GET("/api/v1/platform/imports/{id}/errors.csv", { params: { path: { id } }, parseAs: "blob" });
      return file(r, `import-${id}-errors.csv`);
    },
  };
}

/** Saves a downloaded file through a temporary object URL (no-op where the DOM lacks it). */
export function saveFile({ blob, fileName }: DownloadedFile): void {
  if (typeof URL.createObjectURL !== "function") return;
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = fileName;
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
